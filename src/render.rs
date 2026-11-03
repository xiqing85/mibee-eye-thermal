//! Thermal pseudo-color rendering: To grid (32x24 °C) → ironbow RGB →
//! I420 for the encoder / BMP for the snapshot endpoint.
//!
//! Port of the bring-up viewer pipeline (percentile stretch + ironbow,
//! visual-model QA'd to "acceptable low-res thermal display" standard) with
//! bilinear upscale — the GB28181 placeholder/live stream and the SPEC
//! snapshot share this exact path.

use crate::mlx90640::{HEIGHT, PIXELS, WIDTH};

/// Ironbow control points (t, r, g, b) — same feel as the viewer v4 palette.
const IRONBOW: [(f32, u8, u8, u8); 8] = [
    (0.00, 0, 0, 0),
    (0.15, 18, 8, 64),
    (0.35, 64, 12, 120),
    (0.55, 140, 30, 130),
    (0.72, 216, 88, 60),
    (0.86, 248, 168, 48),
    (0.96, 255, 232, 120),
    (1.00, 255, 255, 224),
];

fn ironbow(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    for w in IRONBOW.windows(2) {
        let (t0, r0, g0, b0) = w[0];
        let (t1, r1, g1, b1) = w[1];
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return (
                (r0 as f32 + f * (r1 as i32 - r0 as i32) as f32) as u8,
                (g0 as f32 + f * (g1 as i32 - g0 as i32) as f32) as u8,
                (b0 as f32 + f * (b1 as i32 - b0 as i32) as f32) as u8,
            );
        }
    }
    let (.., r, g, b) = IRONBOW[IRONBOW.len() - 1];
    (r, g, b)
}

/// Percentile stretch parameters (low/high percentiles, gamma).
const P_LO: f32 = 0.02;
const P_HI: f32 = 0.98;
const GAMMA: f32 = 0.7;

/// Render the sensor grid to an RGB image of `out_w` x `out_h` (bilinear).
/// Auto percentile stretch.
pub fn render_rgb(to: &[f32; PIXELS], out_w: usize, out_h: usize) -> Vec<u8> {
    render_rgb_range(to, out_w, out_h, None, None)
}

/// Render with an optional fixed display range (`lo`..`hi` °C); `None`
/// falls back to the percentile stretch. Fixed ranges keep colors stable
/// across frames (comparable playback) — e.g. `?lo=25&hi=45`.
pub fn render_rgb_range(
    to: &[f32; PIXELS],
    out_w: usize,
    out_h: usize,
    lo_override: Option<f32>,
    hi_override: Option<f32>,
) -> Vec<u8> {
    let (lo, hi) = match (lo_override, hi_override) {
        (Some(lo), Some(hi)) if hi - lo > 1e-3 => (lo, hi),
        _ => {
            let mut sorted = *to;
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (
                sorted[((PIXELS as f32 * P_LO) as usize).min(PIXELS - 1)],
                sorted[((PIXELS as f32 * P_HI) as usize).min(PIXELS - 1)],
            )
        }
    };
    let span = if hi - lo > 1e-3 { hi - lo } else { 1.0 };

    let grid_rgb: Vec<(u8, u8, u8)> = to
        .iter()
        .map(|&v| {
            let t = ((v - lo) / span).clamp(0.0, 1.0).powf(1.0 / GAMMA);
            ironbow(t)
        })
        .collect();

    let mut out = vec![0u8; out_w * out_h * 3];
    for y in 0..out_h {
        // Bilinear source coordinates in the 32x24 grid (pixel centers).
        let gy = (y as f32 + 0.5) * HEIGHT as f32 / out_h as f32 - 0.5;
        let y0 = gy.floor().clamp(0.0, (HEIGHT - 1) as f32) as usize;
        let y1 = (y0 + 1).min(HEIGHT - 1);
        let fy = (gy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..out_w {
            let gx = (x as f32 + 0.5) * WIDTH as f32 / out_w as f32 - 0.5;
            let x0 = gx.floor().clamp(0.0, (WIDTH - 1) as f32) as usize;
            let x1 = (x0 + 1).min(WIDTH - 1);
            let fx = (gx - x0 as f32).clamp(0.0, 1.0);
            let c00 = grid_rgb[y0 * WIDTH + x0];
            let c01 = grid_rgb[y0 * WIDTH + x1];
            let c10 = grid_rgb[y1 * WIDTH + x0];
            let c11 = grid_rgb[y1 * WIDTH + x1];
            let lerp = |a: f32, b: f32, f: f32| a + f * (b - a);
            let ch = |c0: u8, c1: u8, c2: u8, c3: u8| {
                lerp(
                    lerp(f32::from(c0), f32::from(c1), fx),
                    lerp(f32::from(c2), f32::from(c3), fx),
                    fy,
                )
            };
            let (r, g, b) = (
                ch(c00.0, c01.0, c10.0, c11.0),
                ch(c00.1, c01.1, c10.1, c11.1),
                ch(c00.2, c01.2, c10.2, c11.2),
            );
            let o = (y * out_w + x) * 3;
            out[o] = r.clamp(0.0, 255.0) as u8;
            out[o + 1] = g.clamp(0.0, 255.0) as u8;
            out[o + 2] = b.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// RGB (packed, w*h*3) → I420 planar (BT.601 full range).
pub fn rgb_to_i420(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    assert_eq!(rgb.len(), w * h * 3);
    assert!(w % 2 == 0 && h % 2 == 0, "I420 needs even dimensions");
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; w * h / 4];
    let mut v = vec![0u8; w * h / 4];
    for p in 0..w * h {
        let r = f32::from(rgb[p * 3]);
        let g = f32::from(rgb[p * 3 + 1]);
        let b = f32::from(rgb[p * 3 + 2]);
        y[p] = (0.299 * r + 0.587 * g + 0.114 * b)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    for cy in 0..h / 2 {
        for cx in 0..w / 2 {
            let mut us = 0f32;
            let mut vs = 0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = (cy * 2 + dy) * w + cx * 2 + dx;
                    us += -0.169 * f32::from(rgb[p * 3]) - 0.331 * f32::from(rgb[p * 3 + 1])
                        + 0.5 * f32::from(rgb[p * 3 + 2]);
                    vs += 0.5 * f32::from(rgb[p * 3])
                        - 0.419 * f32::from(rgb[p * 3 + 1])
                        - 0.081 * f32::from(rgb[p * 3 + 2]);
                }
            }
            let q = cy * (w / 2) + cx;
            u[q] = ((us / 4.0) + 128.0).round().clamp(0.0, 255.0) as u8;
            v[q] = ((vs / 4.0) + 128.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    out.extend_from_slice(&y);
    out.extend_from_slice(&u);
    out.extend_from_slice(&v);
    out
}

/// Minimal uncompressed 24-bit BMP (row padded to 4 bytes), bottom-up.
pub fn rgb_to_bmp(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut out = vec![0u8; size];
    out[0..2].copy_from_slice(b"BM");
    out[2..6].copy_from_slice(&(size as u32).to_le_bytes());
    out[10..14].copy_from_slice(&54u32.to_le_bytes());
    out[14..18].copy_from_slice(&40u32.to_le_bytes());
    out[18..22].copy_from_slice(&(w as i32).to_le_bytes());
    out[22..26].copy_from_slice(&(h as i32).to_le_bytes());
    out[26..28].copy_from_slice(&1u16.to_le_bytes());
    out[28..30].copy_from_slice(&24u16.to_le_bytes());
    for y in 0..h {
        let src = (h - 1 - y) * w * 3;
        let dst = 54 + y * row;
        out[dst..dst + w * 3].copy_from_slice(&rgb[src..src + w * 3]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ironbow_is_monotonic_and_clamped() {
        // Boundary inputs never panic and endpoints match the table.
        assert_eq!(ironbow(-0.5), ironbow(0.0));
        assert_eq!(ironbow(1.9), ironbow(1.0));
        assert_eq!(ironbow(0.0).0, 0);
        assert_eq!(ironbow(1.0), (255, 255, 224));
        // Heat monotonicity on the luma of a few stops.
        let lum = |t: f32| {
            let (r, g, b) = ironbow(t);
            r as u32 + g as u32 + b as u32
        };
        assert!(lum(0.9) > lum(0.5));
        assert!(lum(0.5) > lum(0.1));
    }

    #[test]
    fn render_sizes_and_i420_layout() {
        let mut to = [25f32; PIXELS];
        to[0] = 40.0; // one hot corner to exercise the stretch
        let rgb = render_rgb(&to, 320, 240);
        assert_eq!(rgb.len(), 320 * 240 * 3);
        let i420 = rgb_to_i420(&rgb, 320, 240);
        assert_eq!(i420.len(), 320 * 240 * 3 / 2);
        let bmp = rgb_to_bmp(&rgb, 320, 240);
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(bmp.len(), 54 + (320 * 3) * 240);
    }
}
