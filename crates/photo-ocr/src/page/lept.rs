//! Small Leptonica routines the layout code relies on, with their exact
//! edge-case behaviour.

use super::bitmap::Bitmap;
use super::morph::LBox;

/// `boxCreate`: boxes reaching into negative coordinates are clipped to the
/// positive quadrant; `None` where Leptonica returns NULL.
pub fn box_create(mut x: i32, mut y: i32, mut w: i32, mut h: i32) -> Option<LBox> {
    if w < 0 || h < 0 {
        return None;
    }
    if x < 0 {
        w += x;
        x = 0;
        if w <= 0 {
            return None;
        }
    }
    if y < 0 {
        h += y;
        y = 0;
        if h <= 0 {
            return None;
        }
    }
    Some(LBox { x, y, w, h })
}

/// `boxClipToRectangle`.
fn box_clip_to_rectangle(b: &LBox, wi: i32, hi: i32) -> Option<LBox> {
    if b.x >= wi || b.y >= hi || b.x + b.w <= 0 || b.y + b.h <= 0 {
        return None;
    }
    let mut d = *b;
    if d.x < 0 {
        d.w += d.x;
        d.x = 0;
    }
    if d.y < 0 {
        d.h += d.y;
        d.y = 0;
    }
    if d.x + d.w > wi {
        d.w = wi - d.x;
    }
    if d.y + d.h > hi {
        d.h = hi - d.y;
    }
    Some(d)
}

#[derive(Clone, Copy)]
enum Scan {
    Left,
    Right,
    Top,
    Bottom,
}

/// `pixScanForForeground`.
fn scan_for_foreground(pix: &Bitmap, b: &LBox, flag: Scan) -> Option<i32> {
    let b = box_clip_to_rectangle(b, pix.width as i32, pix.height as i32)?;
    let (xs, ys, xe, ye) = (b.x, b.y, b.x + b.w - 1, b.y + b.h - 1);
    let on = |x: i32, y: i32| pix.get(x as usize, y as usize);
    match flag {
        Scan::Left => (xs..=xe).find(|&x| (ys..=ye).any(|y| on(x, y))),
        Scan::Right => (xs..=xe).rev().find(|&x| (ys..=ye).any(|y| on(x, y))),
        Scan::Top => (ys..=ye).find(|&y| (xs..=xe).any(|x| on(x, y))),
        Scan::Bottom => (ys..=ye).rev().find(|&y| (xs..=xe).any(|x| on(x, y))),
    }
}

/// `pixClipToForeground` (box only).
fn clip_to_foreground(pix: &Bitmap) -> Option<LBox> {
    let (w, h) = (pix.width as i32, pix.height as i32);
    let whole = LBox { x: 0, y: 0, w, h };
    let miny = scan_for_foreground(pix, &whole, Scan::Top)?;
    let maxy = scan_for_foreground(pix, &whole, Scan::Bottom)?;
    let minx = scan_for_foreground(pix, &whole, Scan::Left)?;
    let maxx = scan_for_foreground(pix, &whole, Scan::Right)?;
    box_create(minx, miny, maxx - minx + 1, maxy - miny + 1)
}

/// `pixClipBoxToForeground(pix, boxs, NULL, &boxd)`; a `None` input box
/// clips the whole image, as Leptonica does for a NULL box.
pub fn clip_box_to_foreground(pix: &Bitmap, b: Option<LBox>) -> Option<LBox> {
    let Some(b) = b else {
        return clip_to_foreground(pix);
    };
    let (w, h) = (pix.width as i32, pix.height as i32);
    let cbw = b.w.min(w - b.x);
    let cbh = b.h.min(h - b.y);
    if cbw < 0 || cbh < 0 {
        return None;
    }
    let t = box_create(b.x, b.y, cbw, cbh)?;
    let left = scan_for_foreground(pix, &t, Scan::Left)?;
    let right = scan_for_foreground(pix, &t, Scan::Right).unwrap_or(0);
    let top = scan_for_foreground(pix, &t, Scan::Top).unwrap_or(0);
    let bottom = scan_for_foreground(pix, &t, Scan::Bottom).unwrap_or(0);
    box_create(left, top, right - left + 1, bottom - top + 1)
}

/// An 8 bpp image, one byte per pixel.
#[derive(Clone, Debug)]
pub struct Gray8 {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Gray8 {
    pub fn new(w: usize, h: usize) -> Gray8 {
        Gray8 {
            w,
            h,
            data: vec![0; w * h],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }
}

/// `pixBlockconv(pix, wc, hc)` on 8 bpp.
pub fn blockconv(src: &Gray8, wc: i32, hc: i32) -> Gray8 {
    let mut d = src.clone();
    blockconv_in_place(&mut d, wc, hc);
    d
}

/// [`blockconv`] writing the result over its input (rows still needed are
/// kept in a ring of `hc + 2` rows).
pub fn blockconv_in_place(d: &mut Gray8, mut wc: i32, mut hc: i32) {
    let (w, h) = (d.w as i32, d.h as i32);
    if wc <= 0 || hc <= 0 {
        return;
    }
    if w < 2 * wc + 1 || h < 2 * hc + 1 {
        wc = wc.min((w - 1) / 2);
        hc = hc.min((h - 1) / 2);
    }
    if wc == 0 || hc == 0 {
        return;
    }
    // pixBlockconvAccum + blockconvLow. Leptonica's value at (i, j) is the
    // accumulator difference acc[imax][jmax] - acc[imax][jmin]
    // + acc[imin][jmin] - acc[imin][jmax], i.e. the exact sum of the source
    // over rows (imin, imax] and columns (jmin, jmax]. A column sum over
    // those rows, moved down row by row, gives the same values without the
    // full-size 32 bpp accumulator.
    let wu = w as usize;
    let wmwc = w - wc;
    let hmhc = h - hc;
    if wmwc <= 0 || hmhc <= 0 {
        d.data.fill(0);
        return;
    }
    let fwc = 2 * wc + 1;
    let fhc = 2 * hc + 1;
    let norm = (1.0 / (f64::from(fwc as f32) * f64::from(fhc))) as f32;
    let mut col = vec![0u32; wu];
    let mut prefix = vec![0u32; wu];
    // Source rows already overwritten by results, while still summed.
    let ring_rows = hc as usize + 2;
    let mut ring = vec![0u8; ring_rows * wu];
    // Rows summed in `col`: lo..=hi (empty while hi < lo).
    let (mut lo, mut hi) = (0usize, None::<usize>);
    for i in 0..h {
        let imin = (i - 1 - hc).max(0) as usize;
        let imax = (i + hc).min(h - 1) as usize;
        let (nlo, nhi) = (imin + 1, imax);
        if let Some(hi) = hi {
            for r in lo..nlo.min(hi + 1) {
                // Row r < i: its source is in the ring.
                let k = r % ring_rows;
                for (c, &v) in col.iter_mut().zip(&ring[k * wu..(k + 1) * wu]) {
                    *c -= u32::from(v);
                }
            }
        }
        let first_new = hi.map_or(nlo, |hi| (hi + 1).max(nlo));
        for r in first_new..=nhi {
            // Row r >= i: not overwritten yet.
            for (c, &v) in col.iter_mut().zip(&d.data[r * wu..(r + 1) * wu]) {
                *c += u32::from(v);
            }
        }
        (lo, hi) = (nlo, Some(nhi));
        let mut run = 0u32;
        for (p, &c) in prefix.iter_mut().zip(&col) {
            run += c;
            *p = run;
        }
        let iu = i as usize;
        let k = iu % ring_rows;
        let out = &mut d.data[iu * wu..(iu + 1) * wu];
        ring[k * wu..(k + 1) * wu].copy_from_slice(out);
        for j in 0..w {
            let jmin = (j - 1 - wc).max(0) as usize;
            let jmax = (j + wc).min(w - 1) as usize;
            let val = prefix[jmax] - prefix[jmin];
            out[j as usize] = (f64::from(norm * val as f32) + 0.5) as u8;
        }
    }
    let scale = |v: u8, f: f32| -> u8 {
        let x = f32::from(v) * f;
        (if x < 255.0 { x } else { 255.0 }) as u8
    };
    let scale2 = |v: u8, f1: f32, f2: f32| -> u8 {
        let x = f32::from(v) * f1 * f2;
        (if x < 255.0 { x } else { 255.0 }) as u8
    };
    let row = |d: &mut Gray8, i: i32, normh: Option<f32>, first_wn_min1: bool| {
        let base = i as usize * wu;
        for j in 0..=wc {
            let wn = if first_wn_min1 {
                (wc + j).max(1)
            } else {
                wc + j
            };
            let normw = fwc as f32 / wn as f32;
            let v = d.data[base + j as usize];
            d.data[base + j as usize] = match normh {
                Some(nh) => scale2(v, nh, normw),
                None => scale(v, normw),
            };
        }
        if let Some(nh) = normh {
            for j in wc + 1..wmwc {
                let v = d.data[base + j as usize];
                d.data[base + j as usize] = scale(v, nh);
            }
        }
        for j in wmwc..w {
            let wn = wc + w - j;
            let normw = fwc as f32 / wn as f32;
            let v = d.data[base + j as usize];
            d.data[base + j as usize] = match normh {
                Some(nh) => scale2(v, nh, normw),
                None => scale(v, normw),
            };
        }
    };
    for i in 0..=hc {
        let hn = (hc + i).max(1);
        let normh = fhc as f32 / hn as f32;
        row(d, i, Some(normh), true);
    }
    for i in hmhc..h {
        let hn = hc + h - i;
        let normh = fhc as f32 / hn as f32;
        row(d, i, Some(normh), false);
    }
    for i in hc + 1..hmhc {
        row(d, i, None, false);
    }
}

#[cfg(test)]
mod blockconv_tests {
    use super::*;

    /// Leptonica's accumulator form (the previous implementation).
    fn reference(src: &Gray8, mut wc: i32, mut hc: i32) -> Gray8 {
        let (w, h) = (src.w as i32, src.h as i32);
        if wc <= 0 || hc <= 0 {
            return src.clone();
        }
        if w < 2 * wc + 1 || h < 2 * hc + 1 {
            wc = wc.min((w - 1) / 2);
            hc = hc.min((h - 1) / 2);
        }
        if wc == 0 || hc == 0 {
            return src.clone();
        }
        // pixBlockconvAccum
        let (wu, hu) = (w as usize, h as usize);
        let mut acc = vec![0u32; wu * hu];
        for j in 0..wu {
            let v = u32::from(src.get(j, 0));
            acc[j] = if j == 0 {
                v
            } else {
                acc[j - 1].wrapping_add(v)
            };
        }
        for i in 1..hu {
            for j in 0..wu {
                let v = u32::from(src.get(j, i));
                acc[i * wu + j] = if j == 0 {
                    v.wrapping_add(acc[(i - 1) * wu])
                } else {
                    v.wrapping_add(acc[i * wu + j - 1])
                        .wrapping_add(acc[(i - 1) * wu + j])
                        .wrapping_sub(acc[(i - 1) * wu + j - 1])
                };
            }
        }
        // blockconvLow
        let mut d = Gray8::new(wu, hu);
        let wmwc = w - wc;
        let hmhc = h - hc;
        if wmwc <= 0 || hmhc <= 0 {
            return d;
        }
        let fwc = 2 * wc + 1;
        let fhc = 2 * hc + 1;
        let norm = (1.0 / (f64::from(fwc as f32) * f64::from(fhc))) as f32;
        for i in 0..h {
            let imin = (i - 1 - hc).max(0) as usize;
            let imax = (i + hc).min(h - 1) as usize;
            for j in 0..w {
                let jmin = (j - 1 - wc).max(0) as usize;
                let jmax = (j + wc).min(w - 1) as usize;
                let val = acc[imax * wu + jmax]
                    .wrapping_sub(acc[imax * wu + jmin])
                    .wrapping_add(acc[imin * wu + jmin])
                    .wrapping_sub(acc[imin * wu + jmax]);
                d.data[i as usize * wu + j as usize] = (f64::from(norm * val as f32) + 0.5) as u8;
            }
        }
        let scale = |v: u8, f: f32| -> u8 {
            let x = f32::from(v) * f;
            (if x < 255.0 { x } else { 255.0 }) as u8
        };
        let scale2 = |v: u8, f1: f32, f2: f32| -> u8 {
            let x = f32::from(v) * f1 * f2;
            (if x < 255.0 { x } else { 255.0 }) as u8
        };
        let row = |d: &mut Gray8, i: i32, normh: Option<f32>, first_wn_min1: bool| {
            let base = i as usize * wu;
            for j in 0..=wc {
                let wn = if first_wn_min1 {
                    (wc + j).max(1)
                } else {
                    wc + j
                };
                let normw = fwc as f32 / wn as f32;
                let v = d.data[base + j as usize];
                d.data[base + j as usize] = match normh {
                    Some(nh) => scale2(v, nh, normw),
                    None => scale(v, normw),
                };
            }
            if let Some(nh) = normh {
                for j in wc + 1..wmwc {
                    let v = d.data[base + j as usize];
                    d.data[base + j as usize] = scale(v, nh);
                }
            }
            for j in wmwc..w {
                let wn = wc + w - j;
                let normw = fwc as f32 / wn as f32;
                let v = d.data[base + j as usize];
                d.data[base + j as usize] = match normh {
                    Some(nh) => scale2(v, nh, normw),
                    None => scale(v, normw),
                };
            }
        };
        for i in 0..=hc {
            let hn = (hc + i).max(1);
            let normh = fhc as f32 / hn as f32;
            row(&mut d, i, Some(normh), true);
        }
        for i in hmhc..h {
            let hn = hc + h - i;
            let normh = fhc as f32 / hn as f32;
            row(&mut d, i, Some(normh), false);
        }
        for i in hc + 1..hmhc {
            row(&mut d, i, None, false);
        }
        d
    }

    #[test]
    fn streaming_matches_accumulator() {
        let mut x = 0x2545_F491u32;
        for (w, h) in [(1, 1), (2, 5), (3, 3), (7, 4), (17, 31), (64, 9), (101, 77)] {
            let data = (0..w * h)
                .map(|i| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    // Mix flat areas, extremes and noise.
                    match i % 7 {
                        0 => 255,
                        1 => 0,
                        _ => (x >> 24) as u8,
                    }
                })
                .collect();
            let src = Gray8 { w, h, data };
            for (wc, hc) in [(0, 1), (1, 0), (1, 1), (2, 1), (1, 3), (5, 5), (40, 2)] {
                let want = reference(&src, wc, hc);
                let got = blockconv(&src, wc, hc);
                assert!(got.data == want.data, "{w}x{h} wc={wc} hc={hc}");
            }
        }
    }
}
