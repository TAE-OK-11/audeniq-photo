//! EXIF orientations that swap the axes (5 TRANSPOSE, 6 ROTATE_270,
//! 7 TRANSVERSE, 8 ROTATE_90 in Pillow's `exif_transpose` terms), applied
//! while converted rows are written, so the unturned image never exists.

/// Source pixel (sx, sy) of a `w`×`h` image lands in output row `sx` (5, 6)
/// or `w-1-sx` (7, 8), at column `sy` (5, 8) or `h-1-sy` (6, 7); the output
/// is `h` pixels wide. `rows` holds source rows `y0..` (`ch` bytes a pixel).
pub(crate) fn place(
    rows: &[u8],
    y0: usize,
    w: usize,
    h: usize,
    ch: usize,
    orientation: u8,
    out: &mut [u8],
) {
    match ch {
        1 => place_n::<1>(rows, y0, w, h, orientation, out),
        3 => place_n::<3>(rows, y0, w, h, orientation, out),
        _ => place_n::<4>(rows, y0, w, h, orientation, out),
    }
}

fn place_n<const N: usize>(
    rows: &[u8],
    y0: usize,
    w: usize,
    h: usize,
    orientation: u8,
    out: &mut [u8],
) {
    let src = rows.as_chunks::<N>().0;
    let dst = out.as_chunks_mut::<N>().0;
    let n = src.len() / w;
    let y1 = y0 + n;
    let row_rev = orientation >= 7;
    let col_rev = orientation == 6 || orientation == 7;
    for sx in 0..w {
        let oy = if row_rev { w - 1 - sx } else { sx };
        let row = &mut dst[oy * h..(oy + 1) * h];
        let column = src.iter().skip(sx).step_by(w);
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
