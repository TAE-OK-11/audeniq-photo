//! EXIF orientation (Pillow `ImageOps.exif_transpose` semantics).
//!
//! Mirrors and the half turn are done in place (no second buffer); the four
//! orientations that swap axes are a cache-tiled transpose followed by an
//! in-place mirror or flip of the result.

use photo_core::Image;

/// Apply orientation 2..=8; other values leave the image unchanged.
pub(crate) fn apply(img: Image, orientation: i64) -> Image {
    match img.format.channels() {
        1 => apply_n::<1>(img, orientation),
        2 => apply_n::<2>(img, orientation),
        3 => apply_n::<3>(img, orientation),
        _ => apply_n::<4>(img, orientation),
    }
}

fn apply_n<const N: usize>(mut img: Image, orientation: i64) -> Image {
    // Pillow transposes: 2 FLIP_LEFT_RIGHT, 3 ROTATE_180, 4 FLIP_TOP_BOTTOM,
    // 5 TRANSPOSE, 6 ROTATE_270, 7 TRANSVERSE, 8 ROTATE_90.
    let (w, h) = (img.width as usize, img.height as usize);
    if !(2..=8).contains(&orientation) || w == 0 || h == 0 {
        return img;
    }
    if orientation <= 4 {
        let px = img.data.as_chunks_mut::<N>().0;
        match orientation {
            2 => mirror(px, w),
            3 => px.reverse(),
            _ => flip(px, w),
        }
        return img;
    }
    let mut out = vec![0u8; img.data.len()];
    rotate::<N>(
        img.data.as_chunks::<N>().0,
        out.as_chunks_mut::<N>().0,
        w,
        h,
        orientation,
    );
    drop(std::mem::take(&mut img.data));
    Image {
        width: h as u32,
        height: w as u32,
        format: img.format,
        data: out,
    }
}

/// Reverse every row of `w` pixels.
fn mirror<T>(px: &mut [T], w: usize) {
    for row in px.chunks_exact_mut(w) {
        row.reverse();
    }
}

/// Swap rows top to bottom.
fn flip<T>(px: &mut [T], w: usize) {
    let h = px.len() / w;
    let (top, bottom) = px.split_at_mut(h / 2 * w);
    let bottom = &mut bottom[(h % 2) * w..];
    for (a, b) in top
        .chunks_exact_mut(w)
        .zip(bottom.chunks_exact_mut(w).rev())
    {
        a.swap_with_slice(b);
    }
}

/// Orientations 5..=8 of `src` (w wide, h tall) into `dst` (h wide, w
/// tall) in one pass over square tiles, so both sides stay in cache.
///
/// Source pixel (sx, sy) lands in output row `sx` (5, 6) or `w-1-sx` (7, 8),
/// at column `sy` (5, 8) or `h-1-sy` (6, 7).
fn rotate<const N: usize>(
    src: &[[u8; N]],
    dst: &mut [[u8; N]],
    w: usize,
    h: usize,
    orientation: i64,
) {
    const TILE: usize = 64;
    let row_rev = orientation >= 7;
    let col_rev = orientation == 6 || orientation == 7;
    for y0 in (0..h).step_by(TILE) {
        let y1 = (y0 + TILE).min(h);
        let band = &src[y0 * w..y1 * w];
        for x0 in (0..w).step_by(TILE) {
            let x1 = (x0 + TILE).min(w);
            for sx in x0..x1 {
                let oy = if row_rev { w - 1 - sx } else { sx };
                let row = &mut dst[oy * h..(oy + 1) * h];
                let column = band.iter().skip(sx).step_by(w);
                if col_rev {
                    for (d, s) in row[h - y1..h - y0].iter_mut().rev().zip(column) {
                        *d = *s;
                    }
                } else {
                    for (d, s) in row[y0..y1].iter_mut().zip(column) {
                        *d = *s;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photo_core::PixelFormat;

    /// The direct per-pixel definition the fast paths must match.
    fn reference(img: &Image, orientation: i64) -> Image {
        let (w, h) = (img.width as usize, img.height as usize);
        let ch = img.format.channels();
        let swap = orientation >= 5;
        let (ow, oh) = if swap { (h, w) } else { (w, h) };
        let mut out = vec![0u8; img.data.len()];
        for y in 0..oh {
            for x in 0..ow {
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                let s = (sy * w + sx) * ch;
                let d = (y * ow + x) * ch;
                out[d..d + ch].copy_from_slice(&img.data[s..s + ch]);
            }
        }
        Image {
            width: ow as u32,
            height: oh as u32,
            format: img.format,
            data: out,
        }
    }

    #[test]
    fn matches_per_pixel_definition() {
        let formats = [
            PixelFormat::Gray8,
            PixelFormat::GrayAlpha8,
            PixelFormat::Rgb8,
            PixelFormat::Rgba8,
        ];
        for format in formats {
            for (w, h) in [(1, 1), (1, 7), (7, 1), (2, 3), (65, 130), (131, 64)] {
                let n = w * h * format.channels();
                let img = Image {
                    width: w as u32,
                    height: h as u32,
                    format,
                    data: (0..n).map(|i| (i * 31 % 251) as u8).collect(),
                };
                for o in 1..=8 {
                    let want = if o == 1 {
                        img.clone()
                    } else {
                        reference(&img, o)
                    };
                    let got = apply(img.clone(), o);
                    assert_eq!((got.width, got.height), (want.width, want.height));
                    assert!(got.data == want.data, "{format:?} {w}x{h} orientation {o}");
                }
            }
        }
    }
}
