//! Huffman tables (`jdhuff.c` derived tables) and the entropy bit reader.

use photo_core::{Error, Result};

const LOOKAHEAD: u32 = 9;

#[derive(Clone)]
pub(crate) struct HuffTable {
    /// `len << 8 | symbol`; 0 when the code is longer than LOOKAHEAD bits.
    lookup: [u16; 1 << LOOKAHEAD],
    maxcode: [i32; 18],
    valoffset: [i32; 18],
    symbols: [u8; 256],
    /// stb_image-style fast AC: `value << 16 | run << 8 | bits` when code
    /// and magnitude fit in the lookahead; 0 otherwise.
    pub(crate) fast_ac: [i32; 1 << LOOKAHEAD],
}

impl HuffTable {
    pub(crate) fn new(counts: &[u8; 16], symbols: &[u8]) -> Result<HuffTable> {
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if total > 256 || total != symbols.len() {
            return Err(Error::Invalid("Huffman table size"));
        }
        let mut t = HuffTable {
            lookup: [0; 1 << LOOKAHEAD],
            maxcode: [-1; 18],
            valoffset: [0; 18],
            symbols: [0; 256],
            fast_ac: [0; 1 << LOOKAHEAD],
        };
        t.symbols[..total].copy_from_slice(symbols);
        let mut code: u32 = 0;
        let mut p = 0usize;
        for l in 1..=16usize {
            let n = counts[l - 1] as usize;
            if n > 0 {
                t.valoffset[l] = p as i32 - code as i32;
                for _ in 0..n {
                    if l as u32 <= LOOKAHEAD {
                        let shift = LOOKAHEAD - l as u32;
                        let entry = ((l as u16) << 8) | u16::from(symbols[p]);
                        for i in 0..(1u32 << shift) {
                            t.lookup[((code << shift) | i) as usize] = entry;
                        }
                    }
                    code += 1;
                    p += 1;
                }
                t.maxcode[l] = code as i32 - 1;
            }
            // Codes must fit in `l` bits (otherwise the table is over-subscribed).
            if code > (1 << l) {
                return Err(Error::Invalid("bad Huffman table"));
            }
            code <<= 1;
        }
        t.maxcode[17] = i32::MAX;
        for i in 0..(1usize << LOOKAHEAD) {
            let e = t.lookup[i];
            if e == 0 {
                continue;
            }
            let len = u32::from(e >> 8);
            let rs = e as u8;
            let (run, size) = (u32::from(rs >> 4), u32::from(rs & 15));
            if size != 0 && len + size <= LOOKAHEAD {
                let v = ((i as u32) >> (LOOKAHEAD - len - size)) & ((1 << size) - 1);
                let v = v as i32;
                let value = if v < (1 << (size - 1)) {
                    v - (1 << size) + 1
                } else {
                    v
                };
                t.fast_ac[i] = (value << 16) | ((run as i32) << 8) | (len + size) as i32;
            }
        }
        Ok(t)
    }
}

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pub(crate) pos: usize,
    buf: u64,
    cnt: u32,
    /// Marker that ended the entropy segment (and its offset).
    pub(crate) marker: Option<(u8, usize)>,
    /// Zero bits supplied after the segment ended.
    fill: u32,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8], pos: usize) -> Self {
        BitReader {
            data,
            pos,
            buf: 0,
            cnt: 0,
            marker: None,
            fill: 0,
        }
    }

    #[inline(always)]
    fn refill(&mut self) {
        if self.marker.is_none() && self.pos + 8 <= self.data.len() {
            let w = u64::from_be_bytes(self.data[self.pos..self.pos + 8].try_into().expect("8"));
            let x = !w;
            // No 0xFF byte among the next eight: take whole bytes at once.
            if x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080 == 0 {
                let k = (64 - self.cnt) / 8;
                if k > 0 {
                    let bytes = w >> (64 - 8 * k);
                    self.buf |= bytes << (64 - self.cnt - 8 * k);
                    self.pos += k as usize;
                    self.cnt += 8 * k;
                }
                return;
            }
        }
        while self.cnt <= 56 {
            let byte = self.next_byte();
            self.buf |= u64::from(byte) << (56 - self.cnt);
            self.cnt += 8;
        }
    }

    #[inline(always)]
    fn next_byte(&mut self) -> u8 {
        if self.marker.is_some() || self.pos >= self.data.len() {
            self.fill += 8;
            return 0;
        }
        let b = self.data[self.pos];
        if b != 0xFF {
            self.pos += 1;
            return b;
        }
        let mut p = self.pos + 1;
        while p < self.data.len() && self.data[p] == 0xFF {
            p += 1;
        }
        if p >= self.data.len() {
            self.pos = self.data.len();
            self.fill += 8;
            return 0;
        }
        if self.data[p] == 0 {
            self.pos = p + 1;
            return 0xFF;
        }
        self.marker = Some((self.data[p], p - 1));
        self.fill += 8;
        0
    }

    /// True if any supplied (fake) bit was consumed: the data ended early.
    #[inline]
    pub(crate) fn overrun(&self) -> bool {
        self.fill > self.cnt
    }

    #[inline(always)]
    pub(crate) fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.cnt < n {
            self.refill();
        }
        let v = (self.buf >> (64 - n)) as u32;
        self.buf <<= n;
        self.cnt -= n;
        v
    }

    /// Peek the lookahead window (refilling first if needed).
    #[inline(always)]
    pub(crate) fn peek_fast(&mut self) -> usize {
        if self.cnt < 16 {
            self.refill();
        }
        (self.buf >> (64 - LOOKAHEAD)) as usize
    }

    #[inline(always)]
    pub(crate) fn skip(&mut self, n: u32) {
        self.buf <<= n;
        self.cnt -= n;
    }

    #[inline(always)]
    pub(crate) fn bit(&mut self) -> u32 {
        self.bits(1)
    }

    #[inline(always)]
    pub(crate) fn decode(&mut self, t: &HuffTable) -> Result<u8> {
        if self.cnt < 16 {
            self.refill();
        }
        let e = t.lookup[(self.buf >> (64 - LOOKAHEAD)) as usize];
        if e != 0 {
            let len = u32::from(e >> 8);
            self.buf <<= len;
            self.cnt -= len;
            return Ok(e as u8);
        }
        let mut l = LOOKAHEAD as usize + 1;
        let mut code = (self.buf >> (64 - l)) as i32;
        while l <= 16 && code > t.maxcode[l] {
            l += 1;
            code = (self.buf >> (64 - l)) as i32;
        }
        if l > 16 {
            return Err(Error::Invalid("corrupt Huffman data"));
        }
        self.buf <<= l;
        self.cnt -= l as u32;
        let idx = code + t.valoffset[l];
        t.symbols
            .get(idx as usize)
            .copied()
            .ok_or(Error::Invalid("corrupt Huffman data"))
    }

    /// `HUFF_EXTEND(get_bits(s), s)`. A magnitude category above 16 can
    /// only come from a corrupt table.
    #[inline(always)]
    pub(crate) fn receive_extend(&mut self, s: u32) -> Result<i32> {
        if s == 0 {
            return Ok(0);
        }
        if s > 16 {
            return Err(Error::Invalid("corrupt Huffman data"));
        }
        let v = self.bits(s) as i32;
        Ok(if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        })
    }

    /// Drop buffered bits and move to the marker ending this segment.
    /// Returns the marker and the offset just past it.
    pub(crate) fn finish_segment(&mut self) -> Result<(u8, usize)> {
        self.buf = 0;
        self.cnt = 0;
        self.fill = 0;
        if let Some((m, at)) = self.marker.take() {
            return Ok((m, at));
        }
        crate::markers::next_marker(self.data, self.pos)
    }

    /// Consume an expected RSTn marker and continue after it.
    pub(crate) fn restart(&mut self, expected: u8) -> Result<()> {
        let (m, at) = self.finish_segment()?;
        if m != 0xD0 + (expected & 7) {
            return Err(Error::Invalid("restart marker mismatch"));
        }
        self.pos = at + 2;
        Ok(())
    }
}
