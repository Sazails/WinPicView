#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assoc;
mod icon;
mod loader;
mod render;
mod text;

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{CursorIcon, Fullscreen, Icon, Theme, Window, WindowId};

use loader::Pic;
use render::View;
use text::Text;

const BG_WINDOWED: u32 = 0x202020;
const BG_FULLSCREEN: u32 = 0x000000;
const CACHE_MAX_ITEMS: usize = 7;
const CACHE_MAX_BYTES: usize = 900 * 1024 * 1024;

enum UserEvent {
    Loaded(PathBuf, Result<Arc<Pic>, String>),
}

type Surf = softbuffer::Surface<Arc<Window>, Arc<Window>>;

struct App {
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<Surf>,
    visible: bool,

    folder: Vec<PathBuf>,
    idx: usize,
    /// Image currently on screen (may lag behind `idx` while loading).
    shown: Option<(PathBuf, Arc<Pic>)>,
    error: Option<String>,
    cache: Vec<(PathBuf, Result<Arc<Pic>, String>)>,
    pending: HashSet<PathBuf>,

    view: View,
    cursor: (f32, f32),
    drag_from: Option<(f32, f32)>,
    last_click: Option<(Instant, (f32, f32))>,
    mods: ModifiersState,

    text: Text,
    show_info: bool,
    show_help: bool,
    toast: Option<(String, Instant)>,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>, start: Option<PathBuf>) -> Self {
        let mut app = App {
            proxy,
            window: None,
            context: None,
            surface: None,
            visible: false,
            folder: Vec::new(),
            idx: 0,
            shown: None,
            error: None,
            cache: Vec::new(),
            pending: HashSet::new(),
            view: View::new(),
            cursor: (0.0, 0.0),
            drag_from: None,
            last_click: None,
            mods: ModifiersState::empty(),
            text: Text::new(),
            show_info: false,
            show_help: false,
            toast: None,
        };
        if let Some(p) = start {
            app.open_path(&p);
        }
        app
    }

    // ---------- folder / navigation ----------

    fn open_path(&mut self, path: &Path) {
        let path = std::fs::canonicalize(path)
            .map(|p| strip_verbatim(&p))
            .unwrap_or_else(|_| path.to_path_buf());
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        self.folder = loader::scan_folder(&dir);
        let same = |a: &PathBuf| a.as_os_str().eq_ignore_ascii_case(path.as_os_str());
        match self.folder.iter().position(same) {
            Some(i) => self.idx = i,
            None => {
                self.folder.push(path.clone());
                self.idx = self.folder.len() - 1;
            }
        }
        self.goto(self.idx);
    }

    fn current_path(&self) -> Option<&PathBuf> {
        self.folder.get(self.idx)
    }

    fn step(&mut self, delta: isize) {
        let n = self.folder.len() as isize;
        if n < 2 {
            return;
        }
        let i = (self.idx as isize + delta).rem_euclid(n) as usize;
        self.goto(i);
    }

    fn goto(&mut self, idx: usize) {
        self.idx = idx;
        let Some(path) = self.folder.get(idx).cloned() else {
            self.shown = None;
            self.update_title();
            self.redraw();
            return;
        };
        self.update_title();
        if let Some(entry) = self.cache.iter().find(|(p, _)| *p == path).map(|(_, r)| r.clone()) {
            self.display(path, entry);
        } else {
            self.request(&path);
        }
    }

    fn display(&mut self, path: PathBuf, result: Result<Arc<Pic>, String>) {
        self.view = View::new();
        match result {
            Ok(pic) => {
                self.error = None;
                self.shown = Some((path, pic));
                self.fit_view();
            }
            Err(e) => {
                self.shown = None;
                self.error = Some(e);
            }
        }
        self.redraw();
        self.show_now();
        self.prefetch();
    }

    fn request(&mut self, path: &Path) {
        if !self.pending.insert(path.to_path_buf()) {
            return;
        }
        let (proxy, path) = (self.proxy.clone(), path.to_path_buf());
        std::thread::spawn(move || {
            let r = loader::load(&path).map(Arc::new);
            let _ = proxy.send_event(UserEvent::Loaded(path, r));
        });
    }

    fn prefetch(&mut self) {
        let n = self.folder.len();
        if n < 2 {
            return;
        }
        for d in [1isize, -1, 2] {
            let p = self.folder[(self.idx as isize + d).rem_euclid(n as isize) as usize].clone();
            if !self.cache.iter().any(|(q, _)| *q == p) {
                self.request(&p);
            }
        }
    }

    fn on_loaded(&mut self, path: PathBuf, result: Result<Arc<Pic>, String>) {
        self.pending.remove(&path);
        self.cache.retain(|(p, _)| *p != path);
        self.cache.push((path.clone(), result.clone()));
        let keep = self.current_path().cloned();
        let mut bytes: usize = self.cache.iter().map(|(_, r)| r.as_ref().map(|p| p.bytes()).unwrap_or(0)).sum();
        while self.cache.len() > 1 && (self.cache.len() > CACHE_MAX_ITEMS || bytes > CACHE_MAX_BYTES) {
            let Some(i) = self.cache.iter().position(|(p, _)| Some(p) != keep.as_ref() && *p != path) else { break };
            bytes -= self.cache[i].1.as_ref().map(|p| p.bytes()).unwrap_or(0);
            let _ = self.cache.remove(i);
        }
        if Some(&path) == keep.as_ref() {
            self.display(path, result);
        }
    }

    fn delete_current(&mut self) {
        let Some(path) = self.current_path().cloned() else { return };
        if !recycle(&path) {
            self.toast("Couldn't move the file to the Recycle Bin");
            return;
        }
        self.cache.retain(|(p, _)| *p != path);
        self.folder.remove(self.idx);
        if self.folder.is_empty() {
            self.shown = None;
            self.error = None;
            self.update_title();
            self.redraw();
        } else {
            self.goto(self.idx.min(self.folder.len() - 1));
        }
        self.toast("Moved to Recycle Bin");
    }

    // ---------- view ----------

    fn win_size(&self) -> (f32, f32) {
        self.window
            .as_ref()
            .map(|w| {
                let s = w.inner_size();
                (s.width as f32, s.height as f32)
            })
            .unwrap_or((1.0, 1.0))
    }

    fn fit_view(&mut self) {
        let win = self.win_size();
        if let Some((_, pic)) = &self.shown {
            self.view.set_fit(pic, win);
        }
    }

    fn zoom_by(&mut self, factor: f32, anchor: (f32, f32)) {
        let win = self.win_size();
        if let Some((_, pic)) = &self.shown {
            self.view.zoom_to(self.view.zoom * factor, anchor, pic, win);
            self.toast_zoom();
            self.redraw();
        }
    }

    fn zoom_exact(&mut self, z: f32, anchor: (f32, f32)) {
        let win = self.win_size();
        if let Some((_, pic)) = &self.shown {
            self.view.zoom_to(z, anchor, pic, win);
            self.toast_zoom();
            self.redraw();
        }
    }

    fn toggle_zoom(&mut self, anchor: (f32, f32)) {
        let win = self.win_size();
        let Some((_, pic)) = &self.shown else { return };
        let fit = self.view.fit_zoom(pic, win);
        if (self.view.zoom - fit).abs() < 1e-3 {
            let target = if fit < 0.999 { 1.0 } else { fit * 2.0 };
            self.zoom_exact(target, anchor);
        } else {
            self.view.set_fit(pic, win);
            self.toast_zoom();
            self.redraw();
        }
    }

    fn rotate(&mut self, cw: bool) {
        if self.shown.is_none() {
            return;
        }
        self.view.rot = (self.view.rot + if cw { 1 } else { 3 }) % 4;
        let win = self.win_size();
        let Some((_, pic)) = &self.shown else { return };
        if self.view.fit {
            self.view.set_fit(pic, win);
        } else {
            self.view.clamp_pan(pic, win);
        }
        self.redraw();
    }

    fn toast(&mut self, s: &str) {
        self.toast = Some((s.to_string(), Instant::now() + Duration::from_millis(1400)));
        self.redraw();
    }

    fn toast_zoom(&mut self) {
        let pct = (self.view.zoom * 100.0).round();
        self.toast(&format!("{pct}%"));
    }

    // ---------- window plumbing ----------

    fn update_title(&self) {
        let Some(w) = &self.window else { return };
        match self.current_path() {
            Some(p) => {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                w.set_title(&format!("{name} ({}/{}) - WinPicView", self.idx + 1, self.folder.len()));
            }
            None => w.set_title("WinPicView"),
        }
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(w) = &self.window else { return };
        if w.fullscreen().is_some() {
            w.set_fullscreen(None);
        } else {
            w.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
    }

    fn open_dialog(&mut self) {
        let exts: Vec<&str> = loader::EXTENSIONS.to_vec();
        let picked = rfd::FileDialog::new().add_filter("Images", &exts).pick_file();
        if let Some(p) = picked {
            self.open_path(&p);
        }
    }

    fn copy_image(&mut self) {
        let Some((_, pic)) = &self.shown else { return };
        let m = &pic.mips[0];
        let ok = arboard::Clipboard::new()
            .and_then(|mut c| {
                c.set_image(arboard::ImageData {
                    width: m.w as usize,
                    height: m.h as usize,
                    bytes: std::borrow::Cow::Borrowed(&m.px),
                })
            })
            .is_ok();
        self.toast(if ok { "Copied to clipboard" } else { "Couldn't copy image" });
    }

    fn reveal_in_explorer(&self) {
        if let Some(p) = self.current_path() {
            let _ = std::process::Command::new("explorer")
                .raw_arg(format!("/select,\"{}\"", p.display()))
                .spawn();
        }
    }

    fn paint(&mut self) {
        let (Some(window), Some(mut surface)) = (self.window.clone(), self.surface.take()) else { return };
        let size = window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
            if surface.resize(w, h).is_ok() {
                if let Ok(mut buf) = surface.buffer_mut() {
                    self.draw(&mut buf, size.width as usize, size.height as usize, window.scale_factor() as f32, window.fullscreen().is_some());
                    let _ = buf.present();
                }
            }
        }
        self.surface = Some(surface);
    }

    /// Paints the first frame while the window is still hidden, then reveals it (no white flash).
    fn show_now(&mut self) {
        if self.visible || self.window.is_none() {
            return;
        }
        self.paint();
        self.visible = true;
        if let Some(w) = &self.window {
            w.set_visible(true);
        }
    }

    fn draw(&mut self, buf: &mut [u32], bw: usize, bh: usize, scale: f32, fullscreen: bool) {
        let bg = if fullscreen { BG_FULLSCREEN } else { BG_WINDOWED };
        buf.fill(bg);
        let px = 15.0 * scale;
        let lh = self.text.line_height(px);
        let (bwf, bhf) = (bw as f32, bh as f32);

        if let Some((_, pic)) = &self.shown {
            render::render(buf, bw, bh, pic, &self.view, bg);
        } else if let Some(err) = &self.error {
            let name = self.current_path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            self.centered(buf, bw, bh, &[format!("Can't open {name}"), err.clone()], px, lh, 0xE0E0E0);
        } else if self.folder.is_empty() {
            self.centered(buf, bw, bh, &["Drop an image here, or press Ctrl+O to open one".to_string()], px, lh, 0xB0B0B0);
        }

        if self.show_info {
            if let (Some((path, pic)), Some(_)) = (&self.shown, self.current_path()) {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let lines = vec![
                    name,
                    format!("{} of {} in folder", self.idx + 1, self.folder.len()),
                    format!("{} x {} px   {}", pic.width(), pic.height(), pic.format),
                    format!("{}   zoom {}%", human_size(pic.file_size), (self.view.zoom * 100.0).round()),
                    path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
                ];
                self.panel(buf, bw, bh, &lines, 16.0 * scale, 16.0 * scale, px, lh);
            }
        }
        if self.show_help {
            let pad = px * 0.9;
            let col = HELP.iter().map(|(k, _)| self.text.measure(k, px)).fold(0.0, f32::max) + px * 1.6;
            let desc = HELP.iter().map(|(_, d)| self.text.measure(d, px)).fold(0.0, f32::max);
            let rows = HELP.len() as f32 + 1.5;
            let (w, h) = (col + desc + pad * 2.0, rows * lh + pad * 2.0);
            let (x, y) = (((bwf - w) / 2.0).max(0.0), ((bhf - h) / 2.0).max(0.0));
            text::fill_rect(buf, bw, bh, x, y, w, h, 0x000000, 205);
            self.text.draw(buf, bw, bh, "Keyboard & mouse", x + pad, y + pad, px, 0xFFFFFF);
            for (i, (k, d)) in HELP.iter().enumerate() {
                let ty = y + pad + (i as f32 + 1.5) * lh;
                self.text.draw(buf, bw, bh, k, x + pad, ty, px, 0xFFFFFF);
                self.text.draw(buf, bw, bh, d, x + pad + col, ty, px, 0xC8C8C8);
            }
        }
        if let Some((msg, _)) = &self.toast {
            let w = self.text.measure(msg, px) + 28.0 * scale;
            let h = lh + 12.0 * scale;
            let (x, y) = ((bwf - w) / 2.0, bhf - h - 36.0 * scale);
            text::fill_rect(buf, bw, bh, x, y, w, h, 0x000000, 190);
            self.text.draw(buf, bw, bh, msg, x + 14.0 * scale, y + 6.0 * scale, px, 0xFFFFFF);
        }
    }

    fn panel(&mut self, buf: &mut [u32], bw: usize, bh: usize, lines: &[String], x: f32, y: f32, px: f32, lh: f32) {
        let pad = px * 0.8;
        let w = lines.iter().map(|l| self.text.measure(l, px)).fold(0.0, f32::max) + pad * 2.0;
        let h = lines.len() as f32 * lh + pad * 2.0;
        text::fill_rect(buf, bw, bh, x, y, w, h, 0x000000, 180);
        for (i, l) in lines.iter().enumerate() {
            let c = if i == 0 { 0xFFFFFF } else { 0xD0D0D0 };
            self.text.draw(buf, bw, bh, l, x + pad, y + pad + i as f32 * lh, px, c);
        }
    }

    fn centered(&mut self, buf: &mut [u32], bw: usize, bh: usize, lines: &[String], px: f32, lh: f32, color: u32) {
        let top = (bh as f32 - lines.len() as f32 * lh) / 2.0;
        for (i, l) in lines.iter().enumerate() {
            let w = self.text.measure(l, px);
            self.text.draw(buf, bw, bh, l, (bw as f32 - w) / 2.0, top + i as f32 * lh, px, color);
        }
    }

    fn on_key(&mut self, key: &Key, repeat: bool, el: &ActiveEventLoop) {
        let ctrl = self.mods.control_key();
        let center = {
            let (w, h) = self.win_size();
            (w / 2.0, h / 2.0)
        };
        match key {
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown | NamedKey::Space) => self.step(1),
            Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp | NamedKey::Backspace) => self.step(-1),
            Key::Named(NamedKey::Home) if !self.folder.is_empty() => self.goto(0),
            Key::Named(NamedKey::End) if !self.folder.is_empty() => self.goto(self.folder.len() - 1),
            Key::Named(NamedKey::Delete) if !repeat => self.delete_current(),
            Key::Named(NamedKey::Escape) => {
                if self.show_help || self.show_info {
                    self.show_help = false;
                    self.show_info = false;
                    self.redraw();
                } else if self.window.as_ref().is_some_and(|w| w.fullscreen().is_some()) {
                    self.toggle_fullscreen();
                } else {
                    el.exit();
                }
            }
            Key::Named(NamedKey::F11 | NamedKey::Enter) if !repeat => self.toggle_fullscreen(),
            Key::Named(NamedKey::F1) if !repeat => {
                self.show_help = !self.show_help;
                self.redraw();
            }
            Key::Character(s) => match s.to_lowercase().as_str() {
                "o" if ctrl && !repeat => self.open_dialog(),
                "c" if ctrl && !repeat => self.copy_image(),
                "e" if ctrl && !repeat => self.reveal_in_explorer(),
                "+" | "=" => self.zoom_by(1.25, center),
                "-" | "_" => self.zoom_by(0.8, center),
                "0" if !repeat => {
                    self.fit_view();
                    self.toast_zoom();
                    self.redraw();
                }
                "1" if !repeat => self.zoom_exact(1.0, center),
                "r" if !repeat => self.rotate(!self.mods.shift_key()),
                "f" if !repeat => self.toggle_fullscreen(),
                "i" if !repeat => {
                    self.show_info = !self.show_info;
                    self.redraw();
                }
                "h" | "?" if !repeat => {
                    self.show_help = !self.show_help;
                    self.redraw();
                }
                _ => {}
            },
            _ => {}
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let rgba = icon::icon_rgba(64);
        let attrs = Window::default_attributes()
            .with_title("WinPicView")
            .with_inner_size(LogicalSize::new(1100.0, 760.0))
            .with_min_inner_size(LogicalSize::new(320.0, 240.0))
            .with_window_icon(Icon::from_rgba(rgba, 64, 64).ok())
            .with_theme(Some(Theme::Dark))
            .with_visible(false);
        let Ok(window) = el.create_window(attrs) else {
            el.exit();
            return;
        };
        let window = Arc::new(window);
        style_title_bar(&window);
        let context = softbuffer::Context::new(window.clone()).expect("graphics context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("surface");
        self.window = Some(window.clone());
        self.context = Some(context);
        self.surface = Some(surface);
        self.update_title();
        self.fit_view();
        if self.folder.is_empty() {
            // Nothing to wait for: show the empty state right away.
            self.show_now();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Loaded(path, r) => self.on_loaded(path, r),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => self.paint(),
            WindowEvent::Resized(_) => {
                let win = self.win_size();
                if let Some((_, pic)) = &self.shown {
                    if self.view.fit {
                        self.view.set_fit(pic, win);
                    } else {
                        self.view.clamp_pan(pic, win);
                    }
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.on_key(&event.logical_key, event.repeat, el);
            }
            WindowEvent::DroppedFile(p) => self.open_path(&p),
            WindowEvent::Focused(true) => {
                // Pick up files added/removed while we were in the background.
                if let Some(cur) = self.current_path().cloned() {
                    if let Some(dir) = cur.parent() {
                        let fresh = loader::scan_folder(dir);
                        if !fresh.is_empty() && fresh != self.folder {
                            self.idx = fresh.iter().position(|p| *p == cur).unwrap_or(self.idx.min(fresh.len() - 1));
                            self.folder = fresh;
                            self.update_title();
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pos = (position.x as f32, position.y as f32);
                if let Some(from) = self.drag_from {
                    let win = self.win_size();
                    if let Some((_, pic)) = &self.shown {
                        self.view.pan.0 += pos.0 - from.0;
                        self.view.pan.1 += pos.1 - from.1;
                        self.view.clamp_pan(pic, win);
                        self.redraw();
                    }
                    self.drag_from = Some(pos);
                }
                self.cursor = pos;
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => {
                    let now = Instant::now();
                    let dbl = self.last_click.is_some_and(|(t, p)| {
                        now.duration_since(t) < Duration::from_millis(350)
                            && (p.0 - self.cursor.0).abs() < 6.0
                            && (p.1 - self.cursor.1).abs() < 6.0
                    });
                    if dbl {
                        self.last_click = None;
                        self.toggle_zoom(self.cursor);
                    } else {
                        self.last_click = Some((now, self.cursor));
                        self.drag_from = Some(self.cursor);
                        if let Some(w) = &self.window {
                            w.set_cursor(CursorIcon::Grabbing);
                        }
                    }
                }
                (MouseButton::Left, ElementState::Released) => {
                    self.drag_from = None;
                    if let Some(w) = &self.window {
                        w.set_cursor(CursorIcon::Grab);
                    }
                }
                (MouseButton::Back, ElementState::Pressed) => self.step(-1),
                (MouseButton::Forward, ElementState::Pressed) => self.step(1),
                _ => {}
            },
            WindowEvent::CursorEntered { .. } => {
                if let Some(w) = &self.window {
                    w.set_cursor(CursorIcon::Grab);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let factor = match delta {
                    MouseScrollDelta::LineDelta(_, y) => 1.2f32.powf(y),
                    MouseScrollDelta::PixelDelta(p) => 1.0025f32.powf(p.y as f32),
                };
                self.zoom_by(factor, self.cursor);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        match &self.toast {
            Some((_, until)) if Instant::now() >= *until => {
                self.toast = None;
                self.redraw();
                el.set_control_flow(ControlFlow::Wait);
            }
            Some((_, until)) => el.set_control_flow(ControlFlow::WaitUntil(*until)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }
}

const HELP: &[(&str, &str)] = &[
    ("Left / Right", "Previous / next image (also PgUp/PgDn, Space, mouse back/forward)"),
    ("Home / End", "First / last image"),
    ("Scroll wheel, + / -", "Zoom at the cursor"),
    ("Drag", "Pan"),
    ("Double-click", "Toggle fit / 100%"),
    ("0 / 1", "Fit to window / actual size"),
    ("R / Shift+R", "Rotate clockwise / counter-clockwise (view only)"),
    ("F, F11, Enter", "Fullscreen"),
    ("I", "Image info"),
    ("Ctrl+O", "Open file"),
    ("Ctrl+C", "Copy image"),
    ("Ctrl+E", "Show in Explorer"),
    ("Delete", "Move to Recycle Bin"),
    ("Esc", "Close panel / leave fullscreen / quit"),
    ("H, F1", "This help"),
];

fn human_size(b: u64) -> String {
    let b = b as f64;
    if b >= 1048576.0 {
        format!("{:.1} MB", b / 1048576.0)
    } else {
        format!("{:.0} KB", (b / 1024.0).max(1.0))
    }
}

fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

/// Moves a file to the Recycle Bin.
fn recycle(path: &Path) -> bool {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW,
    };
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]);
    unsafe {
        let mut op: SHFILEOPSTRUCTW = std::mem::zeroed();
        op.wFunc = FO_DELETE as _;
        op.pFrom = from.as_ptr();
        op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as _;
        SHFileOperationW(&mut op) == 0 && op.fAnyOperationsAborted == 0
    }
}

/// Dark caption colour matching the viewer background (Windows 11).
fn style_title_bar(window: &Window) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let Ok(handle) = window.window_handle() else { return };
    if let RawWindowHandle::Win32(h) = handle.as_raw() {
        let hwnd = h.hwnd.get() as *mut std::ffi::c_void;
        let caption: u32 = 0x0020_2020; // COLORREF 0x00BBGGRR
        let text: u32 = 0x00E0_E0E0;
        unsafe {
            DwmSetWindowAttribute(hwnd, 35, &caption as *const u32 as *const _, 4);
            DwmSetWindowAttribute(hwnd, 36, &text as *const u32 as *const _, 4);
        }
    }
}

fn message(title: &str, body: &str) {
    rfd::MessageDialog::new().set_title(title).set_description(body).show();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--install") => {
            match assoc::install() {
                Ok(p) => {
                    message(
                        "WinPicView",
                        &format!(
                            "Installed to:\n{}\n\nNext: choose WinPicView as the default app for your image types. Settings will open now.",
                            p.display()
                        ),
                    );
                    assoc::open_default_apps_settings();
                }
                Err(e) => message("WinPicView", &format!("Install failed: {e}")),
            }
            return;
        }
        Some("--register") => {
            let r = std::env::current_exe().and_then(|p| assoc::register(&p));
            match r {
                Ok(()) => {
                    message("WinPicView", "Registered. Settings will open so you can choose WinPicView as the default for image types.\n\nKeep this exe where it is; moving it requires registering again.");
                    assoc::open_default_apps_settings();
                }
                Err(e) => message("WinPicView", &format!("Registration failed: {e}")),
            }
            return;
        }
        Some("--unregister") => {
            match assoc::unregister() {
                Ok(()) => message("WinPicView", "File associations removed."),
                Err(e) => message("WinPicView", &format!("Unregister failed: {e}")),
            }
            return;
        }
        _ => {}
    }

    let start = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("event loop");
    let mut app = App::new(event_loop.create_proxy(), start);
    let _ = event_loop.run_app(&mut app);
}
