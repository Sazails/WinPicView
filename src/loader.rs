//! Image decoding, mip-map generation and folder listing.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageDecoder, ImageReader};
use windows_sys::Win32::UI::Shell::StrCmpLogicalW;

/// Extensions we can decode (lowercase, no dot).
pub const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "webp", "gif", "bmp", "dib", "tif", "tiff", "ico", "tga",
    "qoi", "hdr", "exr", "ppm", "pgm", "pbm", "pam",
];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// One level of the mip chain: tightly packed RGBA8.
pub struct Mip {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

pub struct Pic {
    /// mips[0] is full resolution.
    pub mips: Vec<Mip>,
    pub opaque: bool,
    pub file_size: u64,
    pub format: String,
}

impl Pic {
    pub fn width(&self) -> u32 {
        self.mips[0].w
    }
    pub fn height(&self) -> u32 {
        self.mips[0].h
    }
    pub fn bytes(&self) -> usize {
        self.mips.iter().map(|m| m.px.len()).sum()
    }
}

pub fn load(path: &Path) -> Result<Pic, String> {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.no_limits();
    let format = reader
        .format()
        .map(|f| format!("{f:?}").to_uppercase())
        .unwrap_or_else(|| "?".into());
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let px = rgba.into_raw();
    let opaque = px.chunks_exact(4).all(|p| p[3] == 255);
    let mut mips = vec![Mip { w, h, px }];
    while mips.last().map(|m| m.w.max(m.h)).unwrap_or(0) > 32 {
        let next = downscale(mips.last().unwrap(), opaque);
        mips.push(next);
    }
    Ok(Pic { mips, opaque, file_size, format })
}

/// 2x2 box filter (alpha-weighted when the image has transparency).
fn downscale(src: &Mip, opaque: bool) -> Mip {
    let w = (src.w / 2).max(1);
    let h = (src.h / 2).max(1);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let sw = src.w as usize;
    for y in 0..h as usize {
        let y0 = (y * 2).min(src.h as usize - 1);
        let y1 = (y * 2 + 1).min(src.h as usize - 1);
        for x in 0..w as usize {
            let x0 = (x * 2).min(sw - 1);
            let x1 = (x * 2 + 1).min(sw - 1);
            let p = [
                &src.px[(y0 * sw + x0) * 4..][..4],
                &src.px[(y0 * sw + x1) * 4..][..4],
                &src.px[(y1 * sw + x0) * 4..][..4],
                &src.px[(y1 * sw + x1) * 4..][..4],
            ];
            let o = &mut px[(y * w as usize + x) * 4..][..4];
            if opaque {
                for c in 0..3 {
                    o[c] = ((p[0][c] as u32 + p[1][c] as u32 + p[2][c] as u32 + p[3][c] as u32 + 2) / 4) as u8;
                }
                o[3] = 255;
            } else {
                let asum: u32 = p.iter().map(|q| q[3] as u32).sum();
                if asum > 0 {
                    for c in 0..3 {
                        let s: u32 = p.iter().map(|q| q[c] as u32 * q[3] as u32).sum();
                        o[c] = ((s + asum / 2) / asum) as u8;
                    }
                }
                o[3] = ((asum + 2) / 4) as u8;
            }
        }
    }
    Mip { w, h, px }
}

/// Lists supported images in `dir`, in Windows Explorer's natural order.
pub fn scan_folder(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(Vec<u16>, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| !t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| is_supported(p))
        .map(|p| {
            let name = p.file_name().unwrap_or_default();
            let wide: Vec<u16> = name.encode_wide().chain(std::iter::once(0)).collect();
            (wide, p)
        })
        .collect();
    v.sort_by(|a, b| unsafe { StrCmpLogicalW(a.0.as_ptr(), b.0.as_ptr()) }.cmp(&0));
    v.into_iter().map(|(_, p)| p).collect()
}
