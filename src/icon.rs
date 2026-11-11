// SPDX-License-Identifier: AGPL-3.0-or-later
//! The application icon, drawn in code from the same shapes as
//! `assets/icon.svg` so no image decoder is needed.

const BLUE: [f32; 3] = [0.0, 95.0, 184.0];
const WHITE: [f32; 3] = [255.0, 255.0, 255.0];

/// Point inside a rounded rectangle (x0, y0, x1, y1, radius).
fn in_round_rect(x: f32, y: f32, r: (f32, f32, f32, f32, f32)) -> bool {
    let (x0, y0, x1, y1, rad) = r;
    let cx = x.clamp(x0 + rad, x1 - rad);
    let cy = y.clamp(y0 + rad, y1 - rad);
    x >= x0 && x <= x1 && y >= y0 && y <= y1 && (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad
}

fn in_triangle(x: f32, y: f32, t: [(f32, f32); 3]) -> bool {
    let side = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
    let (d1, d2, d3) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

/// Point within `w / 2` of the segment a-b (a stroke with round caps).
fn near_segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32), w: f32) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let (px, py) = (a.0 + t * dx - x, a.1 + t * dy - y);
    px * px + py * py <= w * w / 4.0
}

const LIGHT: [f32; 3] = [156.0, 195.0, 236.0];

/// Point on a circle outline of radius `r` drawn with stroke width `w`.
fn on_ring(x: f32, y: f32, c: (f32, f32), r: f32, w: f32) -> bool {
    (((x - c.0).powi(2) + (y - c.1).powi(2)).sqrt() - r).abs() <= w / 2.0
}

/// Color at a point of the 256x256 design, or None if transparent.
fn sample(x: f32, y: f32) -> Option<[f32; 3]> {
    if !in_round_rect(x, y, (16.0, 16.0, 240.0, 240.0, 52.0)) {
        return None;
    }
    // Page with its top-right corner folded over.
    let fold = in_triangle(x, y, [(160.0, 40.0), (160.0, 76.0), (196.0, 76.0)]);
    if fold {
        return Some(LIGHT);
    }
    let cut = x > 160.0 && y < 76.0;
    let page = !cut && in_round_rect(x, y, (60.0, 40.0, 196.0, 216.0, 14.0));
    if !page {
        return Some(BLUE);
    }
    // "po"
    let po = on_ring(x, y, (100.0, 132.0), 19.0, 13.0)
        || near_segment(x, y, (81.0, 118.0), (81.0, 184.0), 13.0)
        || on_ring(x, y, (156.0, 132.0), 19.0, 13.0);
    Some(if po { BLUE } else { WHITE })
}

/// Renders the icon as straight RGBA with 4x4 supersampling.
pub fn rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let scale = 256.0 / size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut c, mut n) = ([0.0f32; 3], 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) * scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) * scale;
                    if let Some(col) = sample(x, y) {
                        for i in 0..3 {
                            c[i] += col[i];
                        }
                        n += 1;
                    }
                }
            }
            if n == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                // Average over covered samples only (straight alpha).
                out.extend(c.iter().map(|v| (v / n as f32).round() as u8));
                out.push((n * 255 / (SS * SS)) as u8);
            }
        }
    }
    out
}
