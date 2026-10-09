//! 1 bpp images in Leptonica's layout: rows of 32-bit words, most
//! significant bit first, 1 = foreground (black).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub wpl: usize,
    pub data: Vec<u32>,
}

impl Bitmap {
    pub fn new(width: usize, height: usize) -> Bitmap {
        let wpl = width.div_ceil(32);
        Bitmap {
            width,
            height,
            wpl,
            data: vec![0; wpl * height],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        (self.data[y * self.wpl + (x >> 5)] >> (31 - (x & 31))) & 1 != 0
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize) {
        self.data[y * self.wpl + (x >> 5)] |= 0x8000_0000 >> (x & 31);
    }

    #[inline]
    pub fn clear(&mut self, x: usize, y: usize) {
        self.data[y * self.wpl + (x >> 5)] &= !(0x8000_0000 >> (x & 31));
    }

    /// Set pixels in columns `x0..x1` of row `y` (clipped to the width),
    /// counted a word at a time.
    pub fn count_row_span(&self, y: usize, x0: usize, x1: usize) -> u32 {
        let x1 = x1.min(self.width);
        if x0 >= x1 {
            return 0;
        }
        let row = self.row(y);
        let (w0, w1) = (x0 >> 5, (x1 - 1) >> 5);
        // MSB-first: bit for column x is 0x8000_0000 >> (x & 31).
        let head = !0u32 >> (x0 & 31);
        let tail = !0u32 << (31 - ((x1 - 1) & 31));
        if w0 == w1 {
            return (row[w0] & head & tail).count_ones();
        }
        let mut n = (row[w0] & head).count_ones() + (row[w1] & tail).count_ones();
        for &w in &row[w0 + 1..w1] {
            n += w.count_ones();
        }
        n
    }

    pub fn row(&self, y: usize) -> &[u32] {
        &self.data[y * self.wpl..(y + 1) * self.wpl]
    }

    pub fn row_mut(&mut self, y: usize) -> &mut [u32] {
        &mut self.data[y * self.wpl..(y + 1) * self.wpl]
    }

    /// Mask of valid bits in the last word of a row.
    pub fn end_mask(&self) -> u32 {
        let r = self.width & 31;
        if r == 0 { !0 } else { !0u32 << (32 - r) }
    }

    pub fn is_zero(&self) -> bool {
        let em = self.end_mask();
        (0..self.height).all(|y| {
            let row = self.row(y);
            row[..self.wpl - 1].iter().all(|&w| w == 0) && row[self.wpl - 1] & em == 0
        })
    }

    pub fn count(&self) -> u64 {
        let em = self.end_mask();
        let mut n = 0u64;
        for y in 0..self.height {
            let row = self.row(y);
            for (i, &w) in row.iter().enumerate() {
                let w = if i + 1 == self.wpl { w & em } else { w };
                n += u64::from(w.count_ones());
            }
        }
        n
    }

    /// PBM (P4) for debugging and comparisons.
    pub fn to_pbm(&self) -> Vec<u8> {
        let mut out = format!("P4\n{} {}\n", self.width, self.height).into_bytes();
        let bpr = self.width.div_ceil(8);
        for y in 0..self.height {
            let row = self.row(y);
            for b in 0..bpr {
                out.push((row[b / 4] >> (24 - 8 * (b % 4))) as u8);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Bitmap;

    #[test]
    fn count_row_span_matches_pixels() {
        let mut b = Bitmap::new(150, 2);
        let mut seed = 99u32;
        for x in 0..150 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            if seed >> 16 & 1 == 1 {
                b.set(x, 1);
            }
        }
        for x0 in 0..152 {
            for x1 in x0..160 {
                let want = (x0..x1.min(150)).filter(|&x| b.get(x, 1)).count() as u32;
                assert_eq!(b.count_row_span(1, x0, x1), want, "{x0}..{x1}");
            }
        }
    }
}
