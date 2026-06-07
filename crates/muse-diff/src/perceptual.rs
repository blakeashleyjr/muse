//! Pixel diff (§12): per-channel delta, ratio, diff PNG.

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use muse_core::grid::Rect;
use muse_render::font::{CELL_H, CELL_W};

use crate::normalize::MaskRule;

#[derive(Clone, Debug, PartialEq)]
pub struct PixelDiff {
    pub differing: u64,
    pub total: u64,
    pub ratio: f32,
    /// PNG highlighting changed pixels (changed → red, same → dimmed).
    pub diff_png: Vec<u8>,
}

/// Decode a PNG into (w, h, rgba bytes).
pub fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}

fn encode_png(w: u32, h: u32, buf: &[u8]) -> Vec<u8> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let enc = PngEncoder::new_with_quality(&mut out, CompressionType::Best, FilterType::NoFilter);
    enc.write_image(buf, w, h, ExtendedColorType::Rgba8)
        .expect("png encode");
    out
}

/// Fill rect-masked pixel regions with a constant. Rects are in *cell*
/// coordinates; converted to pixels using fixed cell metrics × scale.
fn apply_pixel_masks(buf: &mut [u8], w: u32, h: u32, masks: &[MaskRule], scale: u32) {
    for m in masks {
        if let MaskRule::Rect(Rect {
            row,
            col,
            w: rw,
            h: rh,
        }) = m
        {
            let x0 = *col as u32 * CELL_W * scale;
            let y0 = *row as u32 * CELL_H * scale;
            let x1 = (x0 + *rw as u32 * CELL_W * scale).min(w);
            let y1 = (y0 + *rh as u32 * CELL_H * scale).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    let idx = ((y * w + x) * 4) as usize;
                    if idx + 4 <= buf.len() {
                        buf[idx..idx + 4].copy_from_slice(&[0, 0, 0, 255]);
                    }
                }
            }
        }
    }
}

/// Compare two RGBA buffers. `tolerance` is max per-channel delta still
/// considered equal. Returns differing-pixel stats + a diff PNG.
pub fn diff_rgba(
    w: u32,
    h: u32,
    mut a: Vec<u8>,
    mut b: Vec<u8>,
    tolerance: u8,
    masks: &[MaskRule],
    scale: u32,
) -> PixelDiff {
    apply_pixel_masks(&mut a, w, h, masks, scale);
    apply_pixel_masks(&mut b, w, h, masks, scale);
    let total = (w as u64) * (h as u64);
    let mut differing = 0u64;
    let mut diff_buf = vec![0u8; a.len()];
    for i in 0..total as usize {
        let o = i * 4;
        let da = a[o..o + 3]
            .iter()
            .zip(&b[o..o + 3])
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        if da > tolerance {
            differing += 1;
            diff_buf[o..o + 4].copy_from_slice(&[255, 0, 0, 255]);
        } else {
            // dim the unchanged baseline pixel
            let g = (a[o] as u16 + a[o + 1] as u16 + a[o + 2] as u16) / 3;
            let v = (g / 4) as u8;
            diff_buf[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let ratio = if total == 0 {
        0.0
    } else {
        differing as f32 / total as f32
    };
    PixelDiff {
        differing,
        total,
        ratio,
        diff_png: encode_png(w, h, &diff_buf),
    }
}

/// Compare two PNGs. Returns None if dimensions differ (treated as full diff).
pub fn diff_png(
    baseline: &[u8],
    actual: &[u8],
    tolerance: u8,
    masks: &[MaskRule],
    scale: u32,
) -> Option<PixelDiff> {
    let (wa, ha, ba) = decode_png(baseline)?;
    let (wb, hb, bb) = decode_png(actual)?;
    if wa != wb || ha != hb {
        return None;
    }
    Some(diff_rgba(wa, ha, ba, bb, tolerance, masks, scale))
}

/// Build a solid-color test PNG (used by tests and as a helper).
pub fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let img = RgbaImage::from_pixel(w, h, image::Rgba(rgba));
    encode_png(w, h, img.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_no_diff() {
        let a = vec![10u8; 4 * 4];
        let d = diff_rgba(2, 2, a.clone(), a, 0, &[], 1);
        assert_eq!(d.differing, 0);
        assert_eq!(d.ratio, 0.0);
    }

    #[test]
    fn single_pixel_diff_tolerance_zero() {
        let a = vec![0u8; 4 * 4];
        let mut b = a.clone();
        b[0] = 255;
        let d = diff_rgba(2, 2, a, b, 0, &[], 1);
        assert_eq!(d.differing, 1);
        assert!(d.ratio > 0.0);
        assert!(!d.diff_png.is_empty());
    }

    #[test]
    fn tolerance_absorbs_small_delta() {
        let a = vec![0u8; 4 * 4];
        let mut b = a.clone();
        b[0] = 3;
        let d = diff_rgba(2, 2, a, b, 5, &[], 1);
        assert_eq!(d.differing, 0);
    }

    #[test]
    fn rect_mask_hides_diff() {
        // 8x16 cell => one full cell masked at (0,0)
        let w = CELL_W;
        let h = CELL_H;
        let a = vec![0u8; (w * h * 4) as usize];
        let mut b = a.clone();
        b[0] = 255; // top-left pixel inside masked cell
        let masks = vec![MaskRule::Rect(Rect::new(0, 0, 1, 1))];
        let d = diff_rgba(w, h, a, b, 0, &masks, 1);
        assert_eq!(d.differing, 0);
    }

    #[test]
    fn png_roundtrip_and_diff() {
        let a = solid_png(8, 8, [0, 0, 0, 255]);
        let b = solid_png(8, 8, [0, 0, 0, 255]);
        let d = diff_png(&a, &b, 0, &[], 1).unwrap();
        assert_eq!(d.differing, 0);
        let c = solid_png(8, 8, [255, 255, 255, 255]);
        let d2 = diff_png(&a, &c, 0, &[], 1).unwrap();
        assert_eq!(d2.differing, 64);
    }

    #[test]
    fn png_dimension_mismatch_is_none() {
        let a = solid_png(8, 8, [0, 0, 0, 255]);
        let b = solid_png(16, 8, [0, 0, 0, 255]);
        assert!(diff_png(&a, &b, 0, &[], 1).is_none());
    }

    #[test]
    fn decode_bad_png_none() {
        assert!(decode_png(b"not a png").is_none());
    }

    #[test]
    fn zero_total_ratio() {
        let d = diff_rgba(0, 0, vec![], vec![], 0, &[], 1);
        assert_eq!(d.ratio, 0.0);
    }
}
