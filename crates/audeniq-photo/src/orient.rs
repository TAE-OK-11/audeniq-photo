//! EXIF orientation (Pillow `ImageOps.exif_transpose` semantics).

use photo_core::Image;

/// Apply orientation 2..=8; other values leave the image unchanged.
pub(crate) fn apply(img: Image, orientation: i64) -> Image {
    if !(2..=8).contains(&orientation) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let ch = img.format.channels();
    let swap = orientation >= 5;
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u8; img.data.len()];
    for y in 0..oh {
        for x in 0..ow {
            // Source coordinate for output (x, y), per Pillow transposes:
            // 2 FLIP_LEFT_RIGHT, 3 ROTATE_180, 4 FLIP_TOP_BOTTOM,
            // 5 TRANSPOSE, 6 ROTATE_270, 7 TRANSVERSE, 8 ROTATE_90.
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
    Image { width: ow as u32, height: oh as u32, format: img.format, data: out }
}
