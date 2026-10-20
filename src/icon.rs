//! Procedurally drawn application icon (shared with build.rs).

/// Renders the icon as straight-alpha RGBA at `n`x`n` pixels.
pub fn icon_rgba(n: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = vec![0u8; (n * n * 4) as usize];
    for py in 0..n {
        for px in 0..n {
            let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / n as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / n as f32;
                    if let Some([cr, cg, cb]) = shade(x, y) {
                        r += cr;
                        g += cg;
                        b += cb;
                        a += 1.0;
                    }
                }
            }
            let i = ((py * n + px) * 4) as usize;
            if a > 0.0 {
                out[i] = (r / a) as u8;
                out[i + 1] = (g / a) as u8;
                out[i + 2] = (b / a) as u8;
                out[i + 3] = (a / (SS * SS) as f32 * 255.0) as u8;
            }
        }
    }
    out
}

fn in_rounded_rect(x: f32, y: f32) -> bool {
    let (hx, hy, rad) = (0.46, 0.46, 0.16);
    let qx = (x - 0.5).abs() - (hx - rad);
    let qy = (y - 0.5).abs() - (hy - rad);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) <= rad
}

fn in_tri(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let s = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

fn shade(x: f32, y: f32) -> Option<[f32; 3]> {
    if !in_rounded_rect(x, y) {
        return None;
    }
    let t = y;
    let mut c = [58.0 + (30.0 - 58.0) * t, 134.0 + (91.0 - 134.0) * t, 255.0 + (216.0 - 255.0) * t];
    let (dx, dy) = (x - 0.70, y - 0.30);
    if dx * dx + dy * dy < 0.09 * 0.09 {
        c = [255.0, 214.0, 102.0];
    }
    if in_tri((x, y), (0.40, 0.80), (0.62, 0.50), (0.90, 0.80)) || (y < 0.80 && in_tri((x, y), (0.40, 0.80), (0.62, 0.50), (0.90, 0.80))) {
        c = [207.0, 227.0, 255.0];
    }
    if in_tri((x, y), (0.10, 0.80), (0.36, 0.44), (0.66, 0.80)) {
        c = [245.0, 249.0, 255.0];
    }
    if y >= 0.80 {
        c = [245.0, 249.0, 255.0];
    }
    Some(c)
}

/// A minimal single-image (PNG-compressed) ICO file.
pub fn icon_ico() -> Vec<u8> {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    let n = 256u32;
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&icon_rgba(n), n, n, ExtendedColorType::Rgba8)
        .expect("png encode");
    let mut ico = Vec::with_capacity(22 + png.len());
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // ICONDIR: type=icon, count=1
    ico.extend_from_slice(&[0, 0, 0, 0]); // 256x256 (0 = 256), no palette, reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // planes
    ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // data offset
    ico.extend_from_slice(&png);
    ico
}
