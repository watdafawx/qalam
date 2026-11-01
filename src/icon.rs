//! The app icon (same design as assets/icon.svg: gradient tile, pen nib, squiggle), rasterised at startup
//! so the exe needs no resource compiler.

use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

type P = (f32, f32);

fn rot(p: P, c: P, deg: f32) -> P {
    let (s, co) = deg.to_radians().sin_cos();
    let (x, y) = (p.0 - c.0, p.1 - c.1);
    (c.0 + x * co - y * s, c.1 + x * s + y * co)
}

fn in_poly(p: P, poly: &[P]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn seg_dist(p: P, a: P, b: P) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Colour (r, g, b, a) of the design at point p in 256×256 design space.
fn sample(p: P, nib: &[P], ferrule: &[P], slit: (P, P), dot: P, wave: &[P]) -> (f32, f32, f32, f32) {
    // Rounded tile 8..248, radius 56.
    let c = (p.0.clamp(64.0, 192.0), p.1.clamp(64.0, 192.0));
    if ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt() > 56.0 + 0.0 || !(8.0..248.0).contains(&p.0) || !(8.0..248.0).contains(&p.1) {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let t = ((p.0 + p.1) / 512.0).clamp(0.0, 1.0);
    let mut col = (61.0 + (123.0 - 61.0) * t, 122.0 + (77.0 - 122.0) * t, 240.0 + (255.0 - 240.0) * t);
    let white = (255.0, 255.0, 255.0);
    let accent = (90.0, 99.0, 245.0);
    let mix = |c: (f32, f32, f32), d: (f32, f32, f32), a: f32| (c.0 + (d.0 - c.0) * a, c.1 + (d.1 - c.1) * a, c.2 + (d.2 - c.2) * a);
    if in_poly(p, ferrule) {
        col = mix(col, white, 0.85);
    }
    if in_poly(p, nib) {
        col = white;
        if seg_dist(p, slit.0, slit.1) < 3.5 || ((p.0 - dot.0).powi(2) + (p.1 - dot.1).powi(2)).sqrt() < 9.0 {
            col = accent;
        }
    }
    if wave.windows(2).any(|w| seg_dist(p, w[0], w[1]) < 4.0) {
        col = mix(col, white, 0.9);
    }
    (col.0, col.1, col.2, 255.0)
}

pub fn make(size: i32) -> HICON {
    let n = size.max(16) as usize;
    let px = pixels(n);
    make_from(n, &px)
}

/// The icon at n×n pixels, premultiplied BGRA (0xAARRGGBB).
pub fn pixels(n: usize) -> Vec<u32> {
    let c = (128.0, 112.0);
    let nib: Vec<P> = [(128.0, 44.0), (164.0, 108.0), (148.0, 164.0), (108.0, 164.0), (92.0, 108.0)]
        .iter()
        .map(|&p| rot(p, c, 45.0))
        .collect();
    let ferrule: Vec<P> = [(104.0, 164.0), (152.0, 164.0), (152.0, 180.0), (104.0, 180.0)]
        .iter()
        .map(|&p| rot(p, c, 45.0))
        .collect();
    let slit = (rot((128.0, 92.0), c, 45.0), rot((128.0, 150.0), c, 45.0));
    let dot = rot((128.0, 92.0), c, 45.0);
    // Zigzag approximation of the squiggle from x=52 to 192 at y≈204.
    let wave: Vec<P> = (0..=14).map(|i| (52.0 + i as f32 * 10.0, if i % 2 == 0 { 204.0 } else { 196.0 })).collect();

    let mut px = vec![0u32; n * n];
    const SS: usize = 4;
    for y in 0..n {
        for x in 0..n {
            let mut acc = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let p = (
                        (x as f32 + (sx as f32 + 0.5) / SS as f32) * 256.0 / n as f32,
                        (y as f32 + (sy as f32 + 0.5) / SS as f32) * 256.0 / n as f32,
                    );
                    let (r, g, b, a) = sample(p, &nib, &ferrule, slit, dot, &wave);
                    let a = a / 255.0;
                    acc = (acc.0 + r * a, acc.1 + g * a, acc.2 + b * a, acc.3 + a);
                }
            }
            let k = (SS * SS) as f32;
            let (r, g, b, a) = (acc.0 / k, acc.1 / k, acc.2 / k, acc.3 / k); // premultiplied
            px[y * n + x] = ((a * 255.0) as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
        }
    }
    px
}

fn make_from(n: usize, px: &[u32]) -> HICON {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n as i32,
                biHeight: -(n as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let screen = GetDC(None);
        let color = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        ReleaseDC(None, screen);
        let Ok(color) = color else { return HICON::default() };
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
        let mask = CreateBitmap(n as i32, n as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon
    }
}

/// Writes assets/qalam.ico (embedded in the exe by build.rs). Run after changing the drawing:
/// `cargo test write_ico -- --ignored`
#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn write_ico() {
        let sizes = [16usize, 20, 24, 32, 40, 48, 64, 256];
        let mut images = Vec::new();
        for &n in &sizes {
            let px = super::pixels(n);
            // BMP-style entry: header, bottom-up straight-alpha BGRA rows, then a 1-bit AND mask.
            let mut d = Vec::new();
            for v in [40u32, n as u32, (n * 2) as u32] {
                d.extend_from_slice(&v.to_le_bytes());
            }
            d.extend_from_slice(&1u16.to_le_bytes());
            d.extend_from_slice(&32u16.to_le_bytes());
            d.extend_from_slice(&[0u8; 24]);
            for y in (0..n).rev() {
                for x in 0..n {
                    let v = px[y * n + x];
                    let a = (v >> 24) & 0xff;
                    let un = |c: u32| if a == 0 { 0 } else { (c * 255 / a).min(255) as u8 };
                    d.extend_from_slice(&[un(v & 0xff), un((v >> 8) & 0xff), un((v >> 16) & 0xff), a as u8]);
                }
            }
            d.extend(std::iter::repeat_n(0u8, n.div_ceil(32) * 4 * n));
            images.push((n, d));
        }
        let mut out = Vec::new();
        out.extend_from_slice(&[0, 0, 1, 0]);
        out.extend_from_slice(&(images.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * images.len();
        for (n, d) in &images {
            let b = if *n >= 256 { 0 } else { *n as u8 };
            out.extend_from_slice(&[b, b, 0, 0]);
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&(d.len() as u32).to_le_bytes());
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += d.len();
        }
        for (_, d) in &images {
            out.extend_from_slice(d);
        }
        std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/qalam.ico"), out).unwrap();
    }
}
