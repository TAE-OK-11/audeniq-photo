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
