//! The Leptonica 1 bpp operations Tesseract's page layout uses, with
//! Leptonica's exact semantics (clipping, boundary conditions, ordering).

use super::bitmap::Bitmap;

/// Leptonica `BOX` (x, y from the top-left, width, height).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LBox {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Src,
    Or,
    And,
    /// `PIX_DST & PIX_NOT(PIX_SRC)` (subtract).
    AndNot,
    Xor,
    Clr,
    Set,
}

/// 32 pixels of `row` starting at pixel `start` (MSB first); pixels before
/// 0 or past the row read as 0 (they are masked out by the caller).
#[inline]
fn src_bits(row: &[u32], start: i64) -> u32 {
    if start < 0 {
        let sh = (-start) as u32;
        return if sh >= 32 { 0 } else { src_bits(row, 0) >> sh };
    }
    let wi = (start >> 5) as usize;
    let off = (start & 31) as u32;
    let hi = row.get(wi).copied().unwrap_or(0);
    if off == 0 {
        hi
    } else {
        let lo = row.get(wi + 1).copied().unwrap_or(0);
        (hi << off) | (lo >> (32 - off))
    }
}

impl Bitmap {
    /// `pixRasterop(self, dx, dy, w, h, op, src, sx, sy)` with Leptonica's
    /// clipping: only the part of the rectangle inside both images changes.
    #[allow(clippy::too_many_arguments)]
    pub fn rasterop(
        &mut self,
        dx: i32,
        dy: i32,
        w: i32,
        h: i32,
        op: Op,
        src: Option<&Bitmap>,
        sx: i32,
        sy: i32,
    ) {
        let (dw, dh) = (self.width as i32, self.height as i32);
        let (mut dx, mut dy, mut w, mut h, mut sx, mut sy) = (dx, dy, w, h, sx, sy);
        if let Some(s) = src {
            // Clip to the source.
            if sx < 0 {
                dx -= sx;
                w += sx;
                sx = 0;
            }
            if sy < 0 {
                dy -= sy;
                h += sy;
                sy = 0;
            }
            w = w.min(s.width as i32 - sx);
            h = h.min(s.height as i32 - sy);
        }
        // Clip to the destination.
        if dx < 0 {
            sx -= dx;
            w += dx;
            dx = 0;
        }
        if dy < 0 {
            sy -= dy;
            h += dy;
            dy = 0;
        }
        w = w.min(dw - dx);
        h = h.min(dh - dy);
        if w <= 0 || h <= 0 {
            return;
        }
        let first = (dx >> 5) as usize;
        let last = ((dx + w - 1) >> 5) as usize;
        let (lo, hi) = (i64::from(dx), i64::from(dx + w));
        for y in 0..h {
            let drow = (dy + y) as usize * self.wpl;
            let srow = src.map(|s| s.row((sy + y) as usize));
            for k in first..=last {
                let p0 = (k as i64) * 32;
                // Bits of this word inside [dx, dx + w).
                let a = (lo - p0).max(0) as u32;
                let b = (hi - p0).min(32) as u32;
                let mask = (u32::MAX >> a) & !(u32::MAX.checked_shr(b).unwrap_or(0));
                let s = srow.map_or(0, |row| src_bits(row, p0 - i64::from(dx) + i64::from(sx)));
                let d = self.data[drow + k];
                let v = match op {
                    Op::Src => s,
                    Op::Or => s | d,
                    Op::And => s & d,
                    Op::AndNot => d & !s,
                    Op::Xor => s ^ d,
                    Op::Clr => 0,
                    Op::Set => u32::MAX,
                };
                self.data[drow + k] = (d & !mask) | (v & mask);
            }
        }
    }

    /// `self op= other` over self's full size (Image operators `|=`, `&=`).
    pub fn combine(&mut self, other: &Bitmap, op: Op) {
        let (w, h) = (self.width as i32, self.height as i32);
        self.rasterop(0, 0, w, h, op, Some(other), 0, 0);
    }

    /// `pixSubtract(nullptr, self, other)`.
    pub fn subtract(&self, other: &Bitmap) -> Bitmap {
        let mut d = self.clone();
        d.combine(other, Op::AndNot);
        d
    }

    pub fn and(&self, other: &Bitmap) -> Bitmap {
        let mut d = self.clone();
        d.combine(other, Op::And);
        d
    }

    pub fn or(&self, other: &Bitmap) -> Bitmap {
        let mut d = self.clone();
        d.combine(other, Op::Or);
        d
    }

    /// Brick dilation with a (h × v) sel, origin (h/2, v/2).
    fn dilate_sel(&self, hsize: i32, vsize: i32) -> Bitmap {
        let (cx, cy) = (hsize / 2, vsize / 2);
        let mut d = Bitmap::new(self.width, self.height);
        let (w, h) = (self.width as i32, self.height as i32);
        for i in 0..vsize {
            for j in 0..hsize {
                d.rasterop(j - cx, i - cy, w, h, Op::Or, Some(self), 0, 0);
            }
        }
        d
    }

    /// Brick erosion with asymmetric boundary conditions.
    fn erode_sel(&self, hsize: i32, vsize: i32) -> Bitmap {
        let (cx, cy) = (hsize / 2, vsize / 2);
        let (w, h) = (self.width as i32, self.height as i32);
        let mut d = Bitmap::new(self.width, self.height);
        d.rasterop(0, 0, w, h, Op::Set, None, 0, 0);
        for i in 0..vsize {
            for j in 0..hsize {
                d.rasterop(cx - j, cy - i, w, h, Op::And, Some(self), 0, 0);
            }
        }
        let (xp, yp) = (cx, cy);
        let (xn, yn) = (hsize - 1 - cx, vsize - 1 - cy);
        if xp > 0 {
            d.rasterop(0, 0, xp, h, Op::Clr, None, 0, 0);
        }
        if xn > 0 {
            d.rasterop(w - xn, 0, xn, h, Op::Clr, None, 0, 0);
        }
        if yp > 0 {
            d.rasterop(0, 0, w, yp, Op::Clr, None, 0, 0);
        }
        if yn > 0 {
            d.rasterop(0, h - yn, w, yn, Op::Clr, None, 0, 0);
        }
        d
    }

    pub fn dilate_brick(&self, hsize: i32, vsize: i32) -> Bitmap {
        if hsize == 1 && vsize == 1 {
            return self.clone();
        }
        if hsize == 1 || vsize == 1 {
            return self.dilate_sel(hsize, vsize);
        }
        self.dilate_sel(hsize, 1).dilate_sel(1, vsize)
    }

    pub fn erode_brick(&self, hsize: i32, vsize: i32) -> Bitmap {
        if hsize == 1 && vsize == 1 {
            return self.clone();
        }
        if hsize == 1 || vsize == 1 {
            return self.erode_sel(hsize, vsize);
        }
        self.erode_sel(hsize, 1).erode_sel(1, vsize)
    }

    pub fn open_brick(&self, hsize: i32, vsize: i32) -> Bitmap {
        if hsize == 1 && vsize == 1 {
            return self.clone();
        }
        if hsize == 1 || vsize == 1 {
            return self.erode_sel(hsize, vsize).dilate_sel(hsize, vsize);
        }
        let t = self.erode_sel(hsize, 1);
        let d = t.erode_sel(1, vsize);
        let t = d.dilate_sel(hsize, 1);
        t.dilate_sel(1, vsize)
    }

    pub fn close_brick(&self, hsize: i32, vsize: i32) -> Bitmap {
        if hsize == 1 && vsize == 1 {
            return self.clone();
        }
        if hsize == 1 || vsize == 1 {
            return self.dilate_sel(hsize, vsize).erode_sel(hsize, vsize);
        }
        let t = self.dilate_sel(hsize, 1);
        let d = t.dilate_sel(1, vsize);
        let t = d.erode_sel(hsize, 1);
        t.erode_sel(1, vsize)
    }

    /// `pixCloseSafeBrick`: close with a zero border so erosion does not
    /// clear the edges.
    pub fn close_safe_brick(&self, hsize: i32, vsize: i32) -> Bitmap {
        if hsize == 1 && vsize == 1 {
            return self.clone();
        }
        let maxtrans = (hsize / 2).max(vsize / 2);
        let bord = (32 * ((maxtrans + 31) / 32)) as usize;
        let mut big = Bitmap::new(self.width + 2 * bord, self.height + 2 * bord);
        big.rasterop(
            bord as i32,
            bord as i32,
            self.width as i32,
            self.height as i32,
            Op::Src,
            Some(self),
            0,
            0,
        );
        let closed = big.close_brick(hsize, vsize);
        closed.crop(&LBox {
            x: bord as i32,
            y: bord as i32,
            w: self.width as i32,
            h: self.height as i32,
        })
    }

    /// `pixClipRectangle` (box clipped to the image; empty → None).
    pub fn clip(&self, b: &LBox) -> Option<Bitmap> {
        let x0 = b.x.max(0);
        let y0 = b.y.max(0);
        let x1 = (b.x + b.w).min(self.width as i32);
        let y1 = (b.y + b.h).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(self.crop(&LBox {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }))
    }

    fn crop(&self, b: &LBox) -> Bitmap {
        let mut d = Bitmap::new(b.w as usize, b.h as usize);
        d.rasterop(0, 0, b.w, b.h, Op::Src, Some(self), b.x, b.y);
        d
    }

    pub fn clear_rect(&mut self, b: &LBox) {
        self.rasterop(b.x, b.y, b.w, b.h, Op::Clr, None, 0, 0);
    }

    pub fn set_rect(&mut self, b: &LBox) {
        self.rasterop(b.x, b.y, b.w, b.h, Op::Set, None, 0, 0);
    }

    /// `pixSeedfillBinary(nullptr, seed, mask, conn)`: Leptonica's
    /// word-parallel raster/anti-raster passes, at most 40 iterations, over
    /// the rows and words the seed and mask share (seed pixels outside that
    /// area are kept as they are).
    pub fn seedfill(seed: &Bitmap, mask: &Bitmap, conn: u8) -> Bitmap {
        let mut d = seed.clone();
        let em = mask.end_mask();
        let mrow = |i: usize, j: usize| -> u32 {
            let w = mask.data[i * mask.wpl + j];
            if j + 1 == mask.wpl { w & em } else { w }
        };
        let h = d.height.min(mask.height);
        let wpl = d.wpl.min(mask.wpl);
        let wpls = d.wpl;
        let fill = |mut word: u32, m: u32| -> u32 {
            word &= m;
            if word == 0 || !word == 0 {
                return word;
            }
            loop {
                let prev = word;
                word = (word | (word >> 1) | (word << 1)) & m;
                if word == prev {
                    return word;
                }
            }
        };
        for _ in 0..40 {
            let before = d.data.clone();
            for i in 0..h {
                for j in 0..wpl {
                    let mut word = d.data[i * wpls + j];
                    if i > 0 {
                        let above = d.data[(i - 1) * wpls + j];
                        if conn == 4 {
                            word |= above;
                        } else {
                            word |= above | (above << 1) | (above >> 1);
                            if j > 0 {
                                word |= d.data[(i - 1) * wpls + j - 1] << 31;
                            }
                            if j < wpl - 1 {
                                word |= d.data[(i - 1) * wpls + j + 1] >> 31;
                            }
                        }
                    }
                    if j > 0 {
                        word |= d.data[i * wpls + j - 1] << 31;
                    }
                    d.data[i * wpls + j] = fill(word, mrow(i, j));
                }
            }
            for i in (0..h).rev() {
                for j in (0..wpl).rev() {
                    let mut word = d.data[i * wpls + j];
                    if i < h - 1 {
                        let below = d.data[(i + 1) * wpls + j];
                        if conn == 4 {
                            word |= below;
                        } else {
                            word |= below | (below << 1) | (below >> 1);
                            if j > 0 {
                                word |= d.data[(i + 1) * wpls + j - 1] << 31;
                            }
                            if j < wpl - 1 {
                                word |= d.data[(i + 1) * wpls + j + 1] >> 31;
                            }
                        }
                    }
                    if j < wpl - 1 {
                        word |= d.data[i * wpls + j + 1] >> 31;
                    }
                    d.data[i * wpls + j] = fill(word, mrow(i, j));
                }
            }
            if d.data == before {
                break;
            }
        }
        d
    }

    /// `pixConnComp`: bounding boxes in Leptonica's discovery order (raster
    /// order of each component's first pixel), with optional component
    /// masks (`pixConnCompPixa`, each clipped to its box).
    pub fn conn_comp(&self, conn: u8, want_pix: bool) -> (Vec<LBox>, Vec<Bitmap>) {
        let (w, h) = (self.width, self.height);
        let mut seen = Bitmap::new(w, h);
        let (mut boxes, mut pixes) = (Vec::new(), Vec::new());
        let mut stack = Vec::new();
        let mut comp = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if !self.get(x, y) || seen.get(x, y) {
                    continue;
                }
                seen.set(x, y);
                stack.push((x, y));
                comp.clear();
                let (mut x0, mut y0, mut x1, mut y1) = (x, y, x, y);
                while let Some((cx, cy)) = stack.pop() {
                    comp.push((cx, cy));
                    x0 = x0.min(cx);
                    y0 = y0.min(cy);
                    x1 = x1.max(cx);
                    y1 = y1.max(cy);
                    for (nx, ny) in neighbours(cx, cy, w, h, conn) {
                        if self.get(nx, ny) && !seen.get(nx, ny) {
                            seen.set(nx, ny);
                            stack.push((nx, ny));
                        }
                    }
                }
                let b = LBox {
                    x: x0 as i32,
                    y: y0 as i32,
                    w: (x1 - x0 + 1) as i32,
                    h: (y1 - y0 + 1) as i32,
                };
                if want_pix {
                    let mut p = Bitmap::new(b.w as usize, b.h as usize);
                    for &(cx, cy) in &comp {
                        p.set(cx - x0, cy - y0);
                    }
                    pixes.push(p);
                }
                boxes.push(b);
            }
        }
        (boxes, pixes)
    }

    /// `pixDistanceFunction(pix, 4, 8, L_BOUNDARY_BG)`: one byte per pixel,
    /// row-major without padding.
    pub fn distance_4(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut d: Vec<u8> = (0..w * h)
            .map(|i| u8::from(self.get(i % w, i / w)))
            .collect();
        if w >= 3 && h >= 3 {
            for i in 1..h - 1 {
                for j in 1..w - 1 {
                    if d[i * w + j] > 0 {
                        let m = d[(i - 1) * w + j].min(d[i * w + j - 1]).min(254);
                        d[i * w + j] = m + 1;
                    }
                }
            }
            for i in (1..h - 1).rev() {
                for j in (1..w - 1).rev() {
                    let v = d[i * w + j];
                    if v > 0 {
                        let m = d[(i + 1) * w + j].min(d[i * w + j + 1]);
                        d[i * w + j] = (u16::from(m) + 1).min(u16::from(v)) as u8;
                    }
                }
            }
        }
        d
    }

    /// Maximum of [`Bitmap::distance_4`].
    pub fn max_distance_4(&self) -> i32 {
        self.distance_4().iter().copied().max().map_or(0, i32::from)
    }

    /// `pixReduceRankBinary2`.
    pub fn reduce_rank2(&self, level: u32) -> Bitmap {
        let (wd, hd) = (self.width / 2, self.height / 2);
        let mut d = Bitmap::new(wd, hd);
        for y in 0..hd {
            for x in 0..wd {
                let n = u32::from(self.get(2 * x, 2 * y))
                    + u32::from(self.get(2 * x + 1, 2 * y))
                    + u32::from(self.get(2 * x, 2 * y + 1))
                    + u32::from(self.get(2 * x + 1, 2 * y + 1));
                if n >= level {
                    d.set(x, y);
                }
            }
        }
        d
    }

    /// `pixReduceRankBinaryCascade`.
    pub fn reduce_rank_cascade(&self, levels: [u32; 4]) -> Bitmap {
        if levels[0] == 0 {
            return self.clone();
        }
        let mut p = self.reduce_rank2(levels[0]);
        for &l in &levels[1..] {
            if l == 0 {
                break;
            }
            p = p.reduce_rank2(l);
        }
        p
    }

    /// `pixExpandReplicate` for 1 bpp.
    pub fn expand_replicate(&self, f: usize) -> Bitmap {
        let mut d = Bitmap::new(self.width * f, self.height * f);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.get(x, y) {
                    for dy in 0..f {
                        for dx in 0..f {
                            d.set(x * f + dx, y * f + dy);
                        }
                    }
                }
            }
        }
        d
    }

    /// `pixCountPixels` in a box.
    pub fn count_in(&self, b: &LBox) -> u64 {
        self.clip(b).map_or(0, |p| p.count())
    }
}

fn neighbours(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    conn: u8,
) -> impl Iterator<Item = (usize, usize)> {
    const N8: [(i32, i32); 8] = [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    const N4: [(i32, i32); 4] = [(0, -1), (-1, 0), (1, 0), (0, 1)];
    let list: &'static [(i32, i32)] = if conn == 4 { &N4 } else { &N8 };
    list.iter().filter_map(move |&(dx, dy)| {
        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
        (nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h)
            .then_some((nx as usize, ny as usize))
    })
}

/// `pixGenerateHalftoneMask` (None when the image is under 100 × 100).
pub fn halftone_mask(pixs: &Bitmap) -> Option<Bitmap> {
    if pixs.width < 100 || pixs.height < 100 {
        return None;
    }
    let p1 = pixs.reduce_rank_cascade([4, 4, 0, 0]);
    let p2 = p1.open_brick(5, 5);
    let hs = p2.expand_replicate(4);
    let hm = pixs.close_safe_brick(4, 4);
    Some(Bitmap::seedfill(&hs, &hm, 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixel-by-pixel definition of `pixRasterop` after clipping.
    #[allow(clippy::too_many_arguments)]
    fn rasterop_ref(
        d: &mut Bitmap,
        dx: i32,
        dy: i32,
        w: i32,
        h: i32,
        op: Op,
        src: Option<&Bitmap>,
        sx: i32,
        sy: i32,
    ) {
        let (dw, dh) = (d.width as i32, d.height as i32);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (dx + x, dy + y);
                let (qx, qy) = (sx + x, sy + y);
                if px < 0 || py < 0 || px >= dw || py >= dh {
                    continue;
                }
                let s = match src {
                    Some(s) => {
                        if qx < 0 || qy < 0 || qx >= s.width as i32 || qy >= s.height as i32 {
                            continue;
                        }
                        s.get(qx as usize, qy as usize)
                    }
                    None => false,
                };
                let dv = d.get(px as usize, py as usize);
                let v = match op {
                    Op::Src => s,
                    Op::Or => s | dv,
                    Op::And => s & dv,
                    Op::AndNot => dv & !s,
                    Op::Xor => s ^ dv,
                    Op::Clr => false,
                    Op::Set => true,
                };
                if v {
                    d.set(px as usize, py as usize);
                } else {
                    d.clear(px as usize, py as usize);
                }
            }
        }
    }

    #[test]
    fn word_rasterop_matches_pixel_definition() {
        let mut seed = 12345u64;
        let mut rnd = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for _ in 0..3000 {
            let (w, h) = (1 + rnd(100) as usize, 1 + rnd(12) as usize);
            let mut a = Bitmap::new(w, h);
            let mut s = Bitmap::new(1 + rnd(100) as usize, 1 + rnd(12) as usize);
            for v in a.data.iter_mut().chain(s.data.iter_mut()) {
                *v = rnd(u64::from(u32::MAX)) as u32 ^ (rnd(2) as u32 * 0x8000_0001);
            }
            let ops = [
                Op::Src,
                Op::Or,
                Op::And,
                Op::AndNot,
                Op::Xor,
                Op::Clr,
                Op::Set,
            ];
            let op = ops[rnd(7) as usize];
            let mut r = |n: u64| rnd(n) as i32 - (n as i32) / 2;
            let (dx, dy, rw, rh, sx, sy) = (r(140), r(16), r(260), r(30), r(140), r(16));
            let use_src = r(4) != -2;
            let mut b = a.clone();
            a.rasterop(dx, dy, rw, rh, op, use_src.then_some(&s), sx, sy);
            // Same clipping as the implementation, then the pixel loop.
            let (mut cdx, mut cdy, mut cw, mut ch, mut csx, mut csy) = (dx, dy, rw, rh, sx, sy);
            if use_src {
                if csx < 0 {
                    cdx -= csx;
                    cw += csx;
                    csx = 0;
                }
                if csy < 0 {
                    cdy -= csy;
                    ch += csy;
                    csy = 0;
                }
                cw = cw.min(s.width as i32 - csx);
                ch = ch.min(s.height as i32 - csy);
            }
            if cdx < 0 {
                csx -= cdx;
                cw += cdx;
                cdx = 0;
            }
            if cdy < 0 {
                csy -= cdy;
                ch += cdy;
                cdy = 0;
            }
            cw = cw.min(b.width as i32 - cdx);
            ch = ch.min(b.height as i32 - cdy);
            if cw > 0 && ch > 0 {
                rasterop_ref(
                    &mut b,
                    cdx,
                    cdy,
                    cw,
                    ch,
                    op,
                    use_src.then_some(&s),
                    csx,
                    csy,
                );
            }
            assert_eq!(
                a.data, b.data,
                "{op:?} {dx} {dy} {rw} {rh} {sx} {sy} {use_src}"
            );
        }
    }
}
