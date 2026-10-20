//! View transform math and the CPU image sampler.

use crate::loader::Pic;

#[derive(Clone, Copy)]
pub struct View {
    /// Screen pixels per image pixel.
    pub zoom: f32,
    /// Offset of the image centre from the window centre, in screen pixels.
    pub pan: (f32, f32),
    /// Follow the window size (fit-to-window) when it changes.
    pub fit: bool,
    /// Clockwise quarter turns.
    pub rot: u8,
}

pub const MAX_ZOOM: f32 = 128.0;

impl View {
    pub fn new() -> Self {
        View { zoom: 1.0, pan: (0.0, 0.0), fit: true, rot: 0 }
    }

    /// Image size as displayed (after rotation).
    pub fn dims(&self, pic: &Pic) -> (f32, f32) {
        let (w, h) = (pic.width() as f32, pic.height() as f32);
        if self.rot % 2 == 1 { (h, w) } else { (w, h) }
    }

    /// Zoom that fits the whole image in the window (never enlarges past 100%).
    pub fn fit_zoom(&self, pic: &Pic, win: (f32, f32)) -> f32 {
        let (dw, dh) = self.dims(pic);
        (win.0 / dw).min(win.1 / dh).min(1.0).max(1e-4)
    }

    pub fn min_zoom(&self, pic: &Pic, win: (f32, f32)) -> f32 {
        (self.fit_zoom(pic, win) * 0.5).max(0.005)
    }

    pub fn set_fit(&mut self, pic: &Pic, win: (f32, f32)) {
        self.zoom = self.fit_zoom(pic, win);
        self.pan = (0.0, 0.0);
        self.fit = true;
    }

    /// Keeps the image from being dragged away; centres it on axes where it fits.
    pub fn clamp_pan(&mut self, pic: &Pic, win: (f32, f32)) {
        let (dw, dh) = self.dims(pic);
        let clamp = |size: f32, win: f32, pan: f32| {
            if size <= win { 0.0 } else { pan.clamp(-(size - win) / 2.0, (size - win) / 2.0) }
        };
        self.pan = (
            clamp(dw * self.zoom, win.0, self.pan.0),
            clamp(dh * self.zoom, win.1, self.pan.1),
        );
    }

    /// Zoom so that the image point under `anchor` (window coords) stays put.
    pub fn zoom_to(&mut self, new_zoom: f32, anchor: (f32, f32), pic: &Pic, win: (f32, f32)) {
        let new_zoom = new_zoom.clamp(self.min_zoom(pic, win), MAX_ZOOM);
        let (cx, cy) = (win.0 / 2.0, win.1 / 2.0);
        let ix = (anchor.0 - cx - self.pan.0) / self.zoom;
        let iy = (anchor.1 - cy - self.pan.1) / self.zoom;
        self.zoom = new_zoom;
        self.pan = (anchor.0 - cx - ix * new_zoom, anchor.1 - cy - iy * new_zoom);
        self.fit = false;
        self.clamp_pan(pic, win);
    }
}

pub fn render(buf: &mut [u32], bw: usize, bh: usize, pic: &Pic, view: &View, bg: u32) {
    buf.fill(bg);
    let (dw, dh) = view.dims(pic);
    let z = view.zoom;
    let left = bw as f32 / 2.0 + view.pan.0 - dw * z / 2.0;
    let top = bh as f32 / 2.0 + view.pan.1 - dh * z / 2.0;
    let x0 = (left.ceil().max(0.0)) as usize;
    let x1 = ((left + dw * z).floor().min(bw as f32)).max(0.0) as usize;
    let y0 = (top.ceil().max(0.0)) as usize;
    let y1 = ((top + dh * z).floor().min(bh as f32)).max(0.0) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let level = if z >= 1.0 { 0 } else { ((-z.log2()).floor() as usize).min(pic.mips.len() - 1) };
    let mip = &pic.mips[level];
    let (ow, oh) = (pic.width() as f32, pic.height() as f32);
    // Displayed coords (u,v) -> source coords (sx,sy), as affine rows.
    let (a, b) = match view.rot % 4 {
        0 => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        1 => ([0.0, 1.0, 0.0], [-1.0, 0.0, oh]),
        2 => ([-1.0, 0.0, ow], [0.0, -1.0, oh]),
        _ => ([0.0, -1.0, ow], [1.0, 0.0, 0.0]),
    };
    let (mx, my) = (mip.w as f32 / ow, mip.h as f32 / oh);
    let smooth = z < 3.0;
    let inv = 1.0 / z;
    let bg_rgb = [(bg >> 16) & 255, (bg >> 8) & 255, bg & 255];

    let rows = y1 - y0;
    let threads = if rows * (x1 - x0) > 150_000 {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16)
    } else {
        1
    };
    let per = rows.div_ceil(threads);
    let region = &mut buf[y0 * bw..y1 * bw];

    std::thread::scope(|s| {
        for (ci, chunk) in region.chunks_mut(per * bw).enumerate() {
            let rstart = y0 + ci * per;
            s.spawn(move || {
                for (ri, row) in chunk.chunks_mut(bw).enumerate() {
                    let py = (rstart + ri) as f32 + 0.5;
                    let v = (py - top) * inv;
                    let u0 = (x0 as f32 + 0.5 - left) * inv;
                    let mut sx = (a[0] * u0 + a[1] * v + a[2]) * mx;
                    let mut sy = (b[0] * u0 + b[1] * v + b[2]) * my;
                    let (dsx, dsy) = (a[0] * inv * mx, b[0] * inv * my);
                    for px in &mut row[x0..x1] {
                        let c = if smooth {
                            sample_bilinear(mip, sx - 0.5, sy - 0.5)
                        } else {
                            sample_nearest(mip, sx, sy)
                        };
                        *px = if pic.opaque || c[3] == 255 {
                            (c[0] << 16) | (c[1] << 8) | c[2]
                        } else {
                            let al = c[3];
                            let m = |i: usize| (c[i] * al + bg_rgb[i] * (255 - al) + 127) / 255;
                            (m(0) << 16) | (m(1) << 8) | m(2)
                        };
                        sx += dsx;
                        sy += dsy;
                    }
                }
            });
        }
    });
}

#[inline]
fn sample_nearest(m: &crate::loader::Mip, x: f32, y: f32) -> [u32; 4] {
    let xi = (x.floor().max(0.0) as usize).min(m.w as usize - 1);
    let yi = (y.floor().max(0.0) as usize).min(m.h as usize - 1);
    let p = &m.px[(yi * m.w as usize + xi) * 4..][..4];
    [p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32]
}

#[inline]
fn sample_bilinear(m: &crate::loader::Mip, x: f32, y: f32) -> [u32; 4] {
    let (w, h) = (m.w as usize, m.h as usize);
    let xf = x.floor();
    let yf = y.floor();
    let fx = ((x - xf) * 256.0) as u32;
    let fy = ((y - yf) * 256.0) as u32;
    let x0 = (xf.max(0.0) as usize).min(w - 1);
    let y0 = (yf.max(0.0) as usize).min(h - 1);
    let x1 = ((xf + 1.0).max(0.0) as usize).min(w - 1);
    let y1 = ((yf + 1.0).max(0.0) as usize).min(h - 1);
    let p00 = &m.px[(y0 * w + x0) * 4..][..4];
    let p10 = &m.px[(y0 * w + x1) * 4..][..4];
    let p01 = &m.px[(y1 * w + x0) * 4..][..4];
    let p11 = &m.px[(y1 * w + x1) * 4..][..4];
    let mut o = [0u32; 4];
    for c in 0..4 {
        let top = p00[c] as u32 * (256 - fx) + p10[c] as u32 * fx;
        let bot = p01[c] as u32 * (256 - fx) + p11[c] as u32 * fx;
        o[c] = (top * (256 - fy) + bot * fy + 32768) >> 16;
    }
    o
}
