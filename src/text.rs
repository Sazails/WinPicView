//! Tiny text/overlay drawing on top of the pixel buffer, using the system UI font.

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};

pub struct Text {
    font: Option<Font>,
    cache: HashMap<(char, u32), (Metrics, Vec<u8>)>,
}

impl Text {
    pub fn new() -> Self {
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let font = ["segoeui.ttf", "arial.ttf", "tahoma.ttf"].iter().find_map(|f| {
            let bytes = std::fs::read(format!("{dir}\\Fonts\\{f}")).ok()?;
            Font::from_bytes(bytes, FontSettings::default()).ok()
        });
        Text { font, cache: HashMap::new() }
    }

    fn glyph(&mut self, ch: char, px: f32) -> Option<&(Metrics, Vec<u8>)> {
        let font = self.font.as_ref()?;
        Some(self.cache.entry((ch, px.round() as u32)).or_insert_with(|| font.rasterize(ch, px)))
    }

    pub fn measure(&mut self, s: &str, px: f32) -> f32 {
        s.chars().filter_map(|c| self.glyph(c, px).map(|g| g.0.advance_width)).sum()
    }

    pub fn line_height(&self, px: f32) -> f32 {
        self.font
            .as_ref()
            .and_then(|f| f.horizontal_line_metrics(px))
            .map(|m| m.new_line_size)
            .unwrap_or(px * 1.3)
    }

    /// Draws `s` with its top-left at (x, y).
    pub fn draw(&mut self, buf: &mut [u32], bw: usize, bh: usize, s: &str, x: f32, y: f32, px: f32, color: u32) {
        let Some(ascent) = self
            .font
            .as_ref()
            .and_then(|f| f.horizontal_line_metrics(px))
            .map(|m| m.ascent)
        else {
            return;
        };
        let mut pen = x;
        let base = y + ascent;
        for ch in s.chars() {
            let Some((m, bmp)) = self.glyph(ch, px) else { continue };
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (base - m.height as f32 - m.ymin as f32).round() as i32;
            for row in 0..m.height {
                let dy = gy + row as i32;
                if dy < 0 || dy >= bh as i32 {
                    continue;
                }
                for col in 0..m.width {
                    let dx = gx + col as i32;
                    if dx < 0 || dx >= bw as i32 {
                        continue;
                    }
                    let cov = bmp[row * m.width + col] as u32;
                    if cov > 0 {
                        let d = &mut buf[dy as usize * bw + dx as usize];
                        *d = blend(*d, color, cov);
                    }
                }
            }
            pen += m.advance_width;
        }
    }
}

/// Mixes `src` over `dst` with `alpha` in 0..=255.
pub fn blend(dst: u32, src: u32, alpha: u32) -> u32 {
    let mix = |sh: u32| {
        let d = (dst >> sh) & 255;
        let s = (src >> sh) & 255;
        ((s * alpha + d * (255 - alpha) + 127) / 255) << sh
    };
    mix(16) | mix(8) | mix(0)
}

pub fn fill_rect(buf: &mut [u32], bw: usize, bh: usize, x: f32, y: f32, w: f32, h: f32, color: u32, alpha: u32) {
    let x0 = x.max(0.0) as usize;
    let y0 = y.max(0.0) as usize;
    let x1 = ((x + w).max(0.0) as usize).min(bw);
    let y1 = ((y + h).max(0.0) as usize).min(bh);
    for yy in y0..y1 {
        for p in &mut buf[yy * bw + x0..yy * bw + x1] {
            *p = blend(*p, color, alpha);
        }
    }
}
