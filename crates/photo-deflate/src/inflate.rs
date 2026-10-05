//! DEFLATE decoder.

use crate::checksum::Adler32;
use photo_core::{Error, Result};
use std::sync::OnceLock;

const FAST_BITS: u32 = 10;
const FAST_MASK: u64 = (1 << FAST_BITS) - 1;

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CLEN_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
    /// Zero bytes appended past the end of `data`.
    overrun: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, buf: 0, cnt: 0, overrun: 0 }
    }

    /// Guarantee at least 56 bits in the buffer.
    #[inline(always)]
    fn refill(&mut self) -> Result<()> {
        if self.pos + 8 <= self.data.len() {
            let mut w = [0u8; 8];
            w.copy_from_slice(&self.data[self.pos..self.pos + 8]);
            self.buf |= u64::from_le_bytes(w) << self.cnt;
            self.pos += ((63 - self.cnt) >> 3) as usize;
            self.cnt |= 56;
            Ok(())
        } else {
            self.refill_slow()
        }
    }

    #[cold]
    fn refill_slow(&mut self) -> Result<()> {
        while self.cnt <= 56 {
            let byte = if self.pos < self.data.len() {
                self.pos += 1;
                self.data[self.pos - 1]
            } else {
                self.overrun += 1;
                // A valid stream never needs more than the 8 bytes of
                // lookahead; anything beyond means it ended early.
                if self.overrun > 8 {
                    return Err(Error::Truncated);
                }
                0
            };
            self.buf |= u64::from(byte) << self.cnt;
            self.cnt += 8;
        }
        Ok(())
    }

    #[inline(always)]
    fn consume(&mut self, n: u32) {
        self.buf >>= n;
        self.cnt -= n;
    }

    /// Read `n <= 32` bits (buffer must hold them).
    #[inline(always)]
    fn take(&mut self, n: u32) -> u32 {
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.consume(n);
        v
    }

    fn real_bits_left(&self) -> Result<u32> {
        self.cnt.checked_sub(self.overrun * 8).ok_or(Error::Truncated)
    }

    fn align(&mut self) {
        let r = self.cnt % 8;
        self.consume(r);
    }

    /// Input bytes consumed so far (call after `align`).
    fn consumed(&self) -> Result<usize> {
        let unused = (self.real_bits_left()? / 8) as usize;
        Ok(self.pos - unused)
    }
}

struct Table {
    /// `symbol << 4 | length`; 0 marks codes longer than FAST_BITS.
    fast: [u32; 1 << FAST_BITS],
    count: [u16; 16],
    symbols: [u16; 288],
}

impl Table {
    fn new(lengths: &[u8]) -> Result<Box<Table>> {
        let mut t = Box::new(Table { fast: [0; 1 << FAST_BITS], count: [0; 16], symbols: [0; 288] });
        for &l in lengths {
            t.count[l as usize] += 1;
        }
        t.count[0] = 0;
        // Over-subscribed sets are invalid; incomplete ones are accepted
        // (a lone distance code is legal) and fail only if a hole is used.
        let mut left: i32 = 1;
        for len in 1..16 {
            left = (left << 1) - i32::from(t.count[len]);
            if left < 0 {
                return Err(Error::Invalid("over-subscribed Huffman code"));
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + t.count[len];
        }
        let mut next_code = [0u32; 16];
        let mut code = 0u32;
        for len in 1..16 {
            code = (code + u32::from(t.count[len - 1])) << 1;
            next_code[len] = code;
        }
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = u32::from(l);
            t.symbols[offs[l as usize] as usize] = sym as u16;
            offs[l as usize] += 1;
            let c = next_code[l as usize];
            next_code[l as usize] += 1;
            if l <= FAST_BITS {
                let rev = c.reverse_bits() >> (32 - l);
                let entry = ((sym as u32) << 4) | l;
                let mut i = rev as usize;
                while i < (1 << FAST_BITS) {
                    t.fast[i] = entry;
                    i += 1 << l;
                }
            }
        }
        Ok(t)
    }

    #[inline(always)]
    fn decode(&self, bits: &mut Bits) -> Result<u32> {
        let e = self.fast[(bits.buf & FAST_MASK) as usize];
        if e != 0 {
            bits.consume(e & 15);
            return Ok(e >> 4);
        }
        self.decode_slow(bits)
    }

    #[cold]
    fn decode_slow(&self, bits: &mut Bits) -> Result<u32> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        let mut buf = bits.buf;
        for len in 1..16u32 {
            code |= (buf & 1) as i32;
            buf >>= 1;
            let count = i32::from(self.count[len as usize]);
            if code - count < first {
                bits.consume(len);
                return Ok(u32::from(self.symbols[(index + code - first) as usize]));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::Invalid("invalid Huffman code"))
    }
}

fn fixed_tables() -> &'static (Box<Table>, Box<Table>) {
    static FIXED: OnceLock<(Box<Table>, Box<Table>)> = OnceLock::new();
    FIXED.get_or_init(|| {
        let mut l = [0u8; 288];
        l[..144].fill(8);
        l[144..256].fill(9);
        l[256..280].fill(7);
        l[280..].fill(8);
        let lit = Table::new(&l).expect("fixed literal table");
        let dist = Table::new(&[5u8; 30]).expect("fixed distance table");
        (lit, dist)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The final block ended.
    End,
    /// Output reached the limit in truncating mode.
    Limit,
}

fn inflate_blocks(bits: &mut Bits, out: &mut Vec<u8>, limit: usize, truncate: bool) -> Result<Stop> {
    let start = out.len();
    loop {
        bits.refill()?;
        let last = bits.take(1);
        let kind = bits.take(2);
        let stop = match kind {
            0 => stored(bits, out, limit, truncate)?,
            1 => {
                let (lit, dist) = fixed_tables();
                codes(bits, out, start, limit, truncate, lit, dist)?
            }
            2 => {
                let (lit, dist) = dynamic(bits)?;
                codes(bits, out, start, limit, truncate, &lit, &dist)?
            }
            _ => return Err(Error::Invalid("reserved block type")),
        };
        if stop == Stop::Limit {
            return Ok(Stop::Limit);
        }
        bits.real_bits_left()?;
        if last == 1 {
            return Ok(Stop::End);
        }
    }
}

fn stored(bits: &mut Bits, out: &mut Vec<u8>, limit: usize, truncate: bool) -> Result<Stop> {
    bits.align();
    bits.refill()?;
    let len = bits.take(16) as usize;
    let nlen = bits.take(16) as usize;
    if len != !nlen & 0xFFFF {
        return Err(Error::Invalid("stored block length mismatch"));
    }
    // Drain whole bytes still in the bit buffer, then copy directly.
    let mut remaining = len;
    while remaining > 0 && bits.cnt >= 8 {
        if bits.real_bits_left()? < 8 {
            return Err(Error::Truncated);
        }
        if out.len() >= limit {
            return limit_hit(truncate);
        }
        out.push(bits.take(8) as u8);
        remaining -= 1;
    }
    if remaining > 0 {
        // Bit buffer is empty here, so `pos` is the exact read position.
        debug_assert_eq!(bits.cnt, 0);
        bits.buf = 0;
        let avail = bits.data.len() - bits.pos;
        if remaining > avail {
            return Err(Error::Truncated);
        }
        let room = limit.saturating_sub(out.len());
        let n = remaining.min(room);
        out.extend_from_slice(&bits.data[bits.pos..bits.pos + n]);
        bits.pos += n;
        if n < remaining {
            return limit_hit(truncate);
        }
    }
    Ok(Stop::End)
}

#[inline]
fn limit_hit(truncate: bool) -> Result<Stop> {
    if truncate { Ok(Stop::Limit) } else { Err(Error::Limit("decompressed size")) }
}

fn dynamic(bits: &mut Bits) -> Result<(Box<Table>, Box<Table>)> {
    bits.refill()?;
    let nlen = bits.take(5) as usize + 257;
    let ndist = bits.take(5) as usize + 1;
    let ncode = bits.take(4) as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(Error::Invalid("bad code counts"));
    }
    let mut clens = [0u8; 19];
    // 19 codes need 57 bits; one refill only guarantees 56.
    for (n, &i) in CLEN_ORDER.iter().take(ncode).enumerate() {
        if n % 16 == 0 {
            bits.refill()?;
        }
        clens[i] = bits.take(3) as u8;
    }
    let ctable = Table::new(&clens)?;
    let mut lengths = [0u8; 286 + 30];
    let mut i = 0;
    while i < nlen + ndist {
        bits.refill()?;
        let sym = ctable.decode(bits)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
            continue;
        }
        let (value, repeat) = match sym {
            16 => {
                if i == 0 {
                    return Err(Error::Invalid("repeat with no previous length"));
                }
                (lengths[i - 1], 3 + bits.take(2) as usize)
            }
            17 => (0, 3 + bits.take(3) as usize),
            _ => (0, 11 + bits.take(7) as usize),
        };
        if i + repeat > nlen + ndist {
            return Err(Error::Invalid("too many code lengths"));
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return Err(Error::Invalid("missing end-of-block code"));
    }
    Ok((Table::new(&lengths[..nlen])?, Table::new(&lengths[nlen..nlen + ndist])?))
}

fn codes(
    bits: &mut Bits,
    out: &mut Vec<u8>,
    start: usize,
    limit: usize,
    truncate: bool,
    lit: &Table,
    dist: &Table,
) -> Result<Stop> {
    loop {
        bits.refill()?;
        let sym = lit.decode(bits)?;
        if sym < 256 {
            if out.len() >= limit {
                return limit_hit(truncate);
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(Stop::End);
        }
        let s = (sym - 257) as usize;
        if s >= 29 {
            return Err(Error::Invalid("invalid length symbol"));
        }
        let len = usize::from(LEN_BASE[s]) + bits.take(u32::from(LEN_EXTRA[s])) as usize;
        let d = dist.decode(bits)? as usize;
        if d >= 30 {
            return Err(Error::Invalid("invalid distance symbol"));
        }
        let distance = usize::from(DIST_BASE[d]) + bits.take(u32::from(DIST_EXTRA[d])) as usize;
        if distance > out.len() - start {
            return Err(Error::Invalid("distance too far back"));
        }
        let room = limit.saturating_sub(out.len());
        let n = len.min(room);
        let from = out.len() - distance;
        if distance >= n {
            out.extend_from_within(from..from + n);
        } else if distance == 1 {
            let b = out[from];
            out.resize(out.len() + n, b);
        } else {
            out.reserve(n);
            for k in 0..n {
                let b = out[from + k];
                out.push(b);
            }
        }
        if n < len {
            return limit_hit(truncate);
        }
    }
}

/// Decode a raw DEFLATE stream, appending to `out`. Returns input bytes consumed.
/// Fails with [`Error::Limit`] if `out` would grow beyond `limit` bytes.
pub fn inflate_raw(input: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize> {
    let mut bits = Bits::new(input);
    inflate_blocks(&mut bits, out, limit, false)?;
    bits.align();
    bits.consumed()
}

/// Result of a zlib decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inflated {
    /// Bytes of input consumed, including header and checksum.
    pub consumed: usize,
    /// True when the stream ended (checksum verified); false when decoding
    /// stopped at the output limit in truncating mode.
    pub complete: bool,
}

fn zlib_header(input: &[u8]) -> Result<()> {
    if input.len() < 2 {
        return Err(Error::Truncated);
    }
    let (cmf, flg) = (input[0], input[1]);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 {
        return Err(Error::Invalid("zlib compression method"));
    }
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err(Error::Invalid("zlib header check"));
    }
    if flg & 0x20 != 0 {
        return Err(Error::Unsupported("zlib preset dictionary"));
    }
    Ok(())
}

/// Decode a zlib stream into `out`.
///
/// With `truncate == false` exceeding `limit` is an error. With
/// `truncate == true` decoding stops once `limit` bytes are produced and the
/// result reports `complete: false` (the checksum is not verified then).
pub fn inflate_zlib(input: &[u8], out: &mut Vec<u8>, limit: usize, truncate: bool) -> Result<Inflated> {
    zlib_header(input)?;
    let start = out.len();
    let mut bits = Bits::new(&input[2..]);
    if inflate_blocks(&mut bits, out, limit, truncate)? == Stop::Limit {
        return Ok(Inflated { consumed: 2 + bits.pos, complete: false });
    }
    bits.align();
    let end = 2 + bits.consumed()?;
    let trailer = input.get(end..end + 4).ok_or(Error::Truncated)?;
    let mut adler = Adler32::new();
    adler.update(&out[start..]);
    if adler.finish() != u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]) {
        return Err(Error::Invalid("zlib checksum mismatch"));
    }
    Ok(Inflated { consumed: end + 4, complete: true })
}

/// Decode a complete zlib stream that must produce exactly `expected` bytes
/// and be followed by nothing. Used where the exact bytes are evidence.
pub fn inflate_zlib_exact(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let r = inflate_zlib(input, &mut out, expected, false)?;
    if out.len() != expected || r.consumed != input.len() {
        return Err(Error::Invalid("zlib stream size mismatch"));
    }
    Ok(out)
}
