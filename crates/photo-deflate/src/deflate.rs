//! Streaming DEFLATE encoder: zlib's lazy matcher (`deflate_slow`) with
//! miniz's Moffat–Katajainen Huffman lengths and max-length enforcement.

use crate::checksum::Adler32;

const W: usize = 1 << 15;
const WMASK: usize = W - 1;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const MAX_DIST: usize = W - MIN_LOOKAHEAD;
const TOO_FAR: usize = 4096;
const SYM_BUF: usize = 1 << 14;
const MATCH_FLAG: u32 = 1 << 31;

/// Compression level, 0 (stored) to 9 (best). Same scale as zlib.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level(u8);

impl Level {
    pub const STORE: Level = Level(0);
    pub const FAST: Level = Level(1);
    pub const DEFAULT: Level = Level(6);
    pub const BEST: Level = Level(9);
    pub fn new(level: u8) -> Self {
        Level(level.min(9))
    }
    pub fn get(self) -> u8 {
        self.0
    }
}

impl Default for Level {
    fn default() -> Self {
        Level::DEFAULT
    }
}

#[derive(Clone, Copy)]
struct Config {
    good: usize,
    lazy: usize,
    nice: usize,
    chain: usize,
}

// zlib's configuration_table (levels 1..=9).
const CONFIGS: [Config; 10] = [
    Config {
        good: 0,
        lazy: 0,
        nice: 0,
        chain: 0,
    },
    Config {
        good: 4,
        lazy: 4,
        nice: 8,
        chain: 4,
    },
    Config {
        good: 4,
        lazy: 5,
        nice: 16,
        chain: 8,
    },
    Config {
        good: 4,
        lazy: 6,
        nice: 32,
        chain: 32,
    },
    Config {
        good: 4,
        lazy: 4,
        nice: 16,
        chain: 16,
    },
    Config {
        good: 8,
        lazy: 16,
        nice: 32,
        chain: 32,
    },
    Config {
        good: 8,
        lazy: 16,
        nice: 128,
        chain: 128,
    },
    Config {
        good: 8,
        lazy: 32,
        nice: 128,
        chain: 256,
    },
    Config {
        good: 32,
        lazy: 128,
        nice: 258,
        chain: 1024,
    },
    Config {
        good: 32,
        lazy: 258,
        nice: 258,
        chain: 4096,
    },
];

const fn length_codes() -> [u8; 256] {
    let mut t = [0u8; 256];
    let base: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    let mut code = 0;
    while code < 28 {
        let mut l = base[code];
        while l < base[code + 1] {
            t[(l - 3) as usize] = code as u8;
            l += 1;
        }
        code += 1;
    }
    t[255] = 28;
    t
}

const LEN_CODE: [u8; 256] = length_codes();
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
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

#[inline]
fn dist_code(dist: usize) -> usize {
    // dist in 1..=32768
    let d = (dist - 1) as u32;
    if d < 4 {
        d as usize
    } else {
        let msb = 31 - d.leading_zeros();
        (2 * msb + ((d >> (msb - 1)) & 1)) as usize
    }
}

struct BitWriter {
    buf: u64,
    n: u32,
}

impl BitWriter {
    #[inline(always)]
    fn put(&mut self, out: &mut Vec<u8>, bits: u32, n: u32) {
        debug_assert!(n <= 32);
        self.buf |= u64::from(bits) << self.n;
        self.n += n;
        if self.n >= 32 {
            out.extend_from_slice(&(self.buf as u32).to_le_bytes());
            self.buf >>= 32;
            self.n -= 32;
        }
    }
    fn align(&mut self, out: &mut Vec<u8>) {
        while self.n > 0 {
            out.push(self.buf as u8);
            self.buf >>= 8;
            self.n = self.n.saturating_sub(8);
        }
        self.buf = 0;
    }
}

/// Streaming compressor producing a raw DEFLATE or a zlib stream.
///
/// Matching uses a 4-byte multiplicative hash with chains of absolute
/// positions (no table rebasing when the window slides) and zlib's lazy
/// evaluation rules and per-level limits.
pub struct Compressor {
    level: u8,
    cfg: Config,
    zlib: bool,
    header_done: bool,
    adler: Adler32,
    /// Buffered input; `win[0]` is absolute position `base`.
    win: Vec<u8>,
    base: usize,
    /// Next index in `win` to process.
    pos: usize,
    /// Absolute position + 1 of the latest string per hash (0 = none).
    head: Vec<u32>,
    /// Previous position + 1 in the same chain, indexed by position & WMASK.
    prev: Vec<u32>,
    /// Lazy state: a match (or literal) found at `pos - 1` not yet emitted.
    pending: bool,
    pending_len: usize,
    pending_dist: usize,
    syms: Vec<u32>,
    lit_freq: [u32; 286],
    dist_freq: [u32; 30],
    bits: BitWriter,
    stored: Vec<u8>,
}

const CHUNK: usize = 64 * 1024;
const HASH4_BITS: u32 = 16;

/// Multiplicative hash of the next four bytes. (Hashing three, as zlib
/// does, floods the chains with weak candidates: slower and, on image
/// rows, larger output.)
#[inline(always)]
fn hash4(w: &[u8], p: usize) -> usize {
    let v = u32::from_le_bytes([w[p], w[p + 1], w[p + 2], w[p + 3]]);
    (v.wrapping_mul(0x1E35_A7BD) >> (32 - HASH4_BITS)) as usize
}

impl Compressor {
    /// Raw DEFLATE (no header, no checksum).
    pub fn raw(level: Level) -> Self {
        Self::new(level, false)
    }

    /// zlib-wrapped stream (RFC 1950).
    pub fn zlib(level: Level) -> Self {
        Self::new(level, true)
    }

    fn new(level: Level, zlib: bool) -> Self {
        let l = level.0;
        let lz = l > 0;
        Compressor {
            level: l,
            cfg: CONFIGS[l as usize],
            zlib,
            header_done: false,
            adler: Adler32::new(),
            win: Vec::with_capacity(if lz { 2 * W + CHUNK + MIN_LOOKAHEAD } else { 0 }),
            base: 0,
            pos: 0,
            head: if lz {
                vec![0; 1 << HASH4_BITS]
            } else {
                Vec::new()
            },
            prev: if lz { vec![0; W] } else { Vec::new() },
            pending: false,
            pending_len: 0,
            pending_dist: 0,
            syms: Vec::with_capacity(if lz { SYM_BUF } else { 0 }),
            lit_freq: [0; 286],
            dist_freq: [0; 30],
            bits: BitWriter { buf: 0, n: 0 },
            stored: Vec::new(),
        }
    }

    fn header(&mut self, out: &mut Vec<u8>) {
        if self.zlib && !self.header_done {
            let flevel: u8 = match self.level {
                0 | 1 => 0,
                2..=5 => 1,
                6 => 2,
                _ => 3,
            };
            let cmf = 0x78u8;
            let mut flg = flevel << 6;
            flg += (31 - ((u16::from(cmf) << 8 | u16::from(flg)) % 31) as u8) % 31;
            out.extend_from_slice(&[cmf, flg]);
        }
        self.header_done = true;
    }

    /// Compress `data`, appending any completed output to `out`.
    pub fn write(&mut self, mut data: &[u8], out: &mut Vec<u8>) {
        self.header(out);
        if self.zlib {
            self.adler.update(data);
        }
        if self.level == 0 {
            while !data.is_empty() {
                let n = (65_535 - self.stored.len()).min(data.len());
                self.stored.extend_from_slice(&data[..n]);
                data = &data[n..];
                if self.stored.len() == 65_535 {
                    self.flush_stored(false, out);
                }
            }
            return;
        }
        while !data.is_empty() {
            let n = CHUNK.min(data.len());
            self.win.extend_from_slice(&data[..n]);
            data = &data[n..];
            self.process(false, out);
            self.slide();
        }
    }

    /// Finish the stream (final block and zlib trailer).
    pub fn finish(mut self, out: &mut Vec<u8>) {
        self.header(out);
        if self.level == 0 {
            self.flush_stored(true, out);
        } else {
            self.process(true, out);
            self.flush_block(true, out);
        }
        self.bits.align(out);
        if self.zlib {
            out.extend_from_slice(&self.adler.finish().to_be_bytes());
        }
    }

    fn flush_stored(&mut self, last: bool, out: &mut Vec<u8>) {
        self.bits.put(out, u32::from(last), 1);
        self.bits.put(out, 0, 2);
        self.bits.align(out);
        let len = self.stored.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&self.stored);
        self.stored.clear();
    }

    /// Drop window bytes no match can reach any more.
    fn slide(&mut self) {
        if self.pos >= W + MAX_DIST {
            self.win.drain(..W);
            self.pos -= W;
            self.base += W;
            // Keep absolute positions far from u32 overflow.
            if self.base > (u32::MAX as usize) - (1 << 26) {
                let shift = self.base - W;
                for v in self.head.iter_mut().chain(self.prev.iter_mut()) {
                    *v = (*v as usize).saturating_sub(shift) as u32;
                }
                self.base -= shift;
            }
        }
    }

    #[inline(always)]
    fn insert(head: &mut [u32], prev: &mut [u32], win: &[u8], base: usize, p: usize) -> usize {
        let h = hash4(win, p);
        let abs = base + p + 1;
        let old = head[h];
        prev[(abs - 1) & WMASK] = old;
        head[h] = abs as u32;
        old as usize
    }

    /// Longest match at `p` among chain candidates starting at `cand`
    /// (absolute + 1). Returns (length, distance); length < MIN_MATCH = none.
    #[inline(always)]
    fn longest(&self, p: usize, mut cand: usize, min_len: usize, max_len: usize) -> (usize, usize) {
        let win = &self.win;
        let abs = self.base + p + 1;
        let limit = abs.saturating_sub(MAX_DIST);
        let mut chain = self.cfg.chain;
        if min_len >= self.cfg.good {
            chain >>= 2;
        }
        let nice = self.cfg.nice.min(max_len);
        let mut best = min_len.max(MIN_MATCH - 1);
        let mut best_dist = 0;
        if best >= max_len {
            return (0, 0);
        }
        let cur = &win[p..p + max_len];
        while cand > limit && chain > 0 {
            let ci = cand - 1 - self.base;
            let m = &win[ci..ci + max_len];
            if m[best] == cur[best] && m[0] == cur[0] && m[1] == cur[1] && m[2] == cur[2] {
                let mut len = 0;
                while len + 8 <= max_len {
                    let x = u64::from_le_bytes(m[len..len + 8].try_into().unwrap())
                        ^ u64::from_le_bytes(cur[len..len + 8].try_into().unwrap());
                    if x != 0 {
                        len += (x.trailing_zeros() / 8) as usize;
                        break;
                    }
                    len += 8;
                }
                if len + 8 > max_len {
                    while len < max_len && m[len] == cur[len] {
                        len += 1;
                    }
                }
                if len > best {
                    best = len;
                    best_dist = abs - cand;
                    if len >= nice {
                        break;
                    }
                }
            }
            let next = self.prev[(cand - 1) & WMASK] as usize;
            if next >= cand {
                break;
            }
            cand = next;
            chain -= 1;
        }
        if best_dist == 0 || (best == MIN_MATCH && best_dist > TOO_FAR) {
            (0, 0)
        } else {
            (best, best_dist)
        }
    }

    fn process(&mut self, flush: bool, out: &mut Vec<u8>) {
        let lazy_limit = self.cfg.lazy;
        let greedy = self.level <= 3;
        loop {
            let end = self.win.len();
            let avail = end - self.pos;
            if avail == 0 || (!flush && avail < MIN_LOOKAHEAD) {
                break;
            }
            let p = self.pos;
            let max_len = MAX_MATCH.min(avail);
            let mut cand = 0;
            if avail >= 4 {
                cand = Self::insert(&mut self.head, &mut self.prev, &self.win, self.base, p);
            }
            if greedy {
                let (len, dist) = if cand != 0 && max_len >= MIN_MATCH {
                    self.longest(p, cand, 0, max_len)
                } else {
                    (0, 0)
                };
                if len >= MIN_MATCH {
                    self.matched(dist, len);
                    // zlib deflate_fast: insert inside short matches only.
                    if len <= lazy_limit {
                        for q in p + 1..p + len {
                            if q + 4 <= end {
                                Self::insert(
                                    &mut self.head,
                                    &mut self.prev,
                                    &self.win,
                                    self.base,
                                    q,
                                );
                            }
                        }
                    }
                    self.pos += len;
                } else {
                    let b = self.win[p];
                    self.literal(b);
                    self.pos += 1;
                }
                if self.syms.len() >= SYM_BUF {
                    self.flush_block(false, out);
                }
                continue;
            }
            // Lazy matching (zlib deflate_slow).
            let (len, dist) = if cand != 0
                && max_len >= MIN_MATCH
                && (!self.pending || self.pending_len < lazy_limit)
            {
                self.longest(
                    p,
                    cand,
                    if self.pending { self.pending_len } else { 0 },
                    max_len,
                )
            } else {
                (0, 0)
            };
            if self.pending && self.pending_len >= MIN_MATCH && len <= self.pending_len {
                // Emit the match at p - 1 and skip over it.
                let (plen, pdist) = (self.pending_len, self.pending_dist);
                self.matched(pdist, plen);
                let stop = p - 1 + plen;
                for q in p + 1..stop {
                    if q + 4 <= end {
                        Self::insert(&mut self.head, &mut self.prev, &self.win, self.base, q);
                    }
                }
                self.pos = stop;
                self.pending = false;
            } else {
                if self.pending {
                    let b = self.win[p - 1];
                    self.literal(b);
                }
                self.pending = true;
                self.pending_len = len;
                self.pending_dist = dist;
                self.pos += 1;
            }
            if self.syms.len() >= SYM_BUF {
                self.flush_block(false, out);
            }
        }
        if flush && self.pending {
            let b = self.win[self.pos - 1];
            self.literal(b);
            self.pending = false;
        }
    }

    #[inline(always)]
    fn literal(&mut self, b: u8) {
        self.lit_freq[usize::from(b)] += 1;
        self.syms.push(u32::from(b));
    }

    #[inline(always)]
    fn matched(&mut self, dist: usize, len: usize) {
        let lc = usize::from(LEN_CODE[len - MIN_MATCH]);
        self.lit_freq[257 + lc] += 1;
        self.dist_freq[dist_code(dist)] += 1;
        self.syms
            .push(MATCH_FLAG | ((dist as u32) << 8) | (len - MIN_MATCH) as u32);
    }

    fn flush_block(&mut self, last: bool, out: &mut Vec<u8>) {
        self.lit_freq[256] += 1;
        let mut lit_len = [0u8; 286];
        let mut dist_len = [0u8; 30];
        build_lengths(&self.lit_freq, 15, &mut lit_len);
        // At least two distance codes keep strict decoders happy.
        let mut dfreq = self.dist_freq;
        let used = dfreq.iter().filter(|&&f| f > 0).count();
        if used < 2 {
            if dfreq[0] == 0 {
                dfreq[0] = 1;
            }
            if used == 0 || dfreq[1] == 0 {
                if dfreq[1] == 0 {
                    dfreq[1] = 1;
                } else {
                    dfreq[0] = dfreq[0].max(1);
                }
            }
        }
        build_lengths(&dfreq, 15, &mut dist_len);

        let hlit = 257.max(286 - lit_len.iter().rev().take_while(|&&l| l == 0).count());
        let hdist = 1.max(30 - dist_len.iter().rev().take_while(|&&l| l == 0).count());
        let mut all = Vec::with_capacity(hlit + hdist);
        all.extend_from_slice(&lit_len[..hlit]);
        all.extend_from_slice(&dist_len[..hdist]);
        let rle = rle_lengths(&all);
        let mut cl_freq = [0u32; 19];
        for &(sym, _) in &rle {
            cl_freq[usize::from(sym)] += 1;
        }
        let mut cl_len = [0u8; 19];
        build_lengths(&cl_freq, 7, &mut cl_len);
        let hclen = 4.max(
            19 - CLEN_ORDER
                .iter()
                .rev()
                .take_while(|&&i| cl_len[i] == 0)
                .count(),
        );

        let mut dyn_bits: u64 = 14 + 3 * hclen as u64;
        for &(sym, _) in &rle {
            dyn_bits += u64::from(cl_len[usize::from(sym)])
                + match sym {
                    16 => 2,
                    17 => 3,
                    18 => 7,
                    _ => 0,
                };
        }
        let mut fixed_bits: u64 = 0;
        for (i, &f) in self.lit_freq.iter().enumerate() {
            dyn_bits += u64::from(f) * u64::from(lit_len[i]);
            fixed_bits += u64::from(f) * fixed_lit_len(i);
        }
        for (i, &f) in self.dist_freq.iter().enumerate() {
            dyn_bits += u64::from(f) * u64::from(dist_len[i]);
            fixed_bits += u64::from(f) * 5;
        }

        let bw = &mut self.bits;
        bw.put(out, u32::from(last), 1);
        let (lcodes, dcodes);
        if fixed_bits <= dyn_bits {
            bw.put(out, 1, 2);
            let mut fl = [0u8; 288];
            for (i, l) in fl.iter_mut().enumerate() {
                *l = fixed_lit_len(i) as u8;
            }
            lcodes = canonical(&fl, &mut lit_len_full());
            dcodes = canonical(&[5u8; 30], &mut [0u8; 30]);
        } else {
            bw.put(out, 2, 2);
            bw.put(out, (hlit - 257) as u32, 5);
            bw.put(out, (hdist - 1) as u32, 5);
            bw.put(out, (hclen - 4) as u32, 4);
            for &i in CLEN_ORDER.iter().take(hclen) {
                bw.put(out, u32::from(cl_len[i]), 3);
            }
            let (clc, _) = canonical(&cl_len, &mut [0u8; 19]);
            for &(sym, extra) in &rle {
                let s = usize::from(sym);
                bw.put(out, clc[s], u32::from(cl_len[s]));
                match sym {
                    16 => bw.put(out, u32::from(extra), 2),
                    17 => bw.put(out, u32::from(extra), 3),
                    18 => bw.put(out, u32::from(extra), 7),
                    _ => {}
                }
            }
            lcodes = canonical(&lit_len, &mut lit_len_full());
            dcodes = canonical(&dist_len, &mut [0u8; 30]);
        }
        let (lc, ll) = lcodes;
        let (dc, dl) = dcodes;
        for &s in &self.syms {
            if s & MATCH_FLAG == 0 {
                let i = s as usize;
                bw.put(out, lc[i], u32::from(ll[i]));
            } else {
                let len_m3 = (s & 0xFF) as usize;
                let dist = ((s & !MATCH_FLAG) >> 8) as usize;
                let code = usize::from(LEN_CODE[len_m3]);
                bw.put(out, lc[257 + code], u32::from(ll[257 + code]));
                let e = u32::from(LEN_EXTRA[code]);
                if e > 0 {
                    bw.put(
                        out,
                        (len_m3 + MIN_MATCH - usize::from(LEN_BASE[code])) as u32,
                        e,
                    );
                }
                let d = dist_code(dist);
                bw.put(out, dc[d], u32::from(dl[d]));
                let e = u32::from(DIST_EXTRA[d]);
                if e > 0 {
                    bw.put(out, (dist - usize::from(DIST_BASE[d])) as u32, e);
                }
            }
        }
        bw.put(out, lc[256], u32::from(ll[256]));
        self.syms.clear();
        self.lit_freq = [0; 286];
        self.dist_freq = [0; 30];
    }
}

fn lit_len_full() -> [u8; 288] {
    [0u8; 288]
}

fn fixed_lit_len(i: usize) -> u64 {
    match i {
        0..=143 => 8,
        144..=255 => 9,
        256..=279 => 7,
        _ => 8,
    }
}

/// Bit-reversed canonical codes for `lengths`; also returns the lengths
/// copied into `store` (sized to the alphabet) for convenient indexing.
fn canonical<const N: usize>(lengths: &[u8], store: &mut [u8; N]) -> ([u32; N], [u8; N]) {
    let mut count = [0u32; 16];
    for &l in lengths {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    let mut codes = [0u32; N];
    for (i, &l) in lengths.iter().enumerate() {
        store[i] = l;
        if l > 0 {
            let c = next[usize::from(l)];
            next[usize::from(l)] += 1;
            codes[i] = c.reverse_bits() >> (32 - u32::from(l));
        }
    }
    (codes, *store)
}

/// Code-length alphabet RLE (zlib's `send_tree`): (symbol, extra bits value).
fn rle_lengths(l: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::with_capacity(l.len());
    let mut i = 0;
    while i < l.len() {
        let cur = l[i];
        let mut run = 1;
        while i + run < l.len() && l[i + run] == cur {
            run += 1;
        }
        i += run;
        if cur == 0 {
            while run >= 11 {
                let r = run.min(138);
                out.push((18, (r - 11) as u8));
                run -= r;
            }
            if run >= 3 {
                out.push((17, (run - 3) as u8));
                run = 0;
            }
        } else {
            out.push((cur, 0));
            run -= 1;
            while run >= 3 {
                let r = run.min(6);
                out.push((16, (r - 3) as u8));
                run -= r;
            }
        }
        for _ in 0..run {
            out.push((cur, 0));
        }
    }
    out
}

/// Length-limited Huffman code lengths for `freq` (miniz algorithm).
fn build_lengths(freq: &[u32], max_len: usize, out: &mut [u8]) {
    out.iter_mut().for_each(|l| *l = 0);
    let mut syms: Vec<(u64, usize)> = freq
        .iter()
        .enumerate()
        .filter(|&(_, &f)| f > 0)
        .map(|(i, &f)| (u64::from(f), i))
        .collect();
    match syms.len() {
        0 => return,
        1 => {
            out[syms[0].1] = 1;
            return;
        }
        _ => {}
    }
    syms.sort_unstable();
    let n = syms.len();
    let mut a: Vec<u64> = syms.iter().map(|s| s.0).collect();
    // Moffat & Katajainen, in place.
    a[0] += a[1];
    let (mut root, mut leaf) = (0usize, 2usize);
    for next in 1..n - 1 {
        if leaf >= n || a[root] < a[leaf] {
            a[next] = a[root];
            a[root] = next as u64;
            root += 1;
        } else {
            a[next] = a[leaf];
            leaf += 1;
        }
        if leaf >= n || (root < next && a[root] < a[leaf]) {
            a[next] += a[root];
            a[root] = next as u64;
            root += 1;
        } else {
            a[next] += a[leaf];
            leaf += 1;
        }
    }
    a[n - 2] = 0;
    for next in (0..n.saturating_sub(2)).rev() {
        a[next] = a[a[next] as usize] + 1;
    }
    let (mut avbl, mut used, mut dpth) = (1i64, 0i64, 0u64);
    let mut root = n as i64 - 2;
    let mut next = n as i64 - 1;
    while avbl > 0 {
        while root >= 0 && a[root as usize] == dpth {
            used += 1;
            root -= 1;
        }
        while avbl > used {
            a[next as usize] = dpth;
            next -= 1;
            avbl -= 1;
        }
        avbl = 2 * used;
        dpth += 1;
        used = 0;
    }
    let mut num = vec![0u32; n + 2];
    for &l in &a {
        num[l as usize] += 1;
    }
    // Enforce the maximum code length.
    if n > 1 {
        let mut over = 0;
        for i in max_len + 1..num.len() {
            over += num[i];
            num[i] = 0;
        }
        if num.len() <= max_len {
            num.resize(max_len + 1, 0);
        }
        num[max_len] += over;
        let mut total: u64 = 0;
        for i in (1..=max_len).rev() {
            total += u64::from(num[i]) << (max_len - i);
        }
        while total != 1 << max_len {
            num[max_len] -= 1;
            for i in (1..max_len).rev() {
                if num[i] > 0 {
                    num[i] -= 1;
                    num[i + 1] += 2;
                    break;
                }
            }
            total -= 1;
        }
    }
    let mut j = n;
    for (len, &count) in num.iter().enumerate().take(max_len + 1).skip(1) {
        for _ in 0..count {
            j -= 1;
            out[syms[j].1] = len as u8;
        }
    }
}

/// One-shot zlib compression.
pub fn compress_zlib(data: &[u8], level: Level) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    let mut c = Compressor::zlib(level);
    c.write(data, &mut out);
    c.finish(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::{inflate_raw, inflate_zlib};

    fn roundtrip(data: &[u8], level: Level) {
        let z = compress_zlib(data, level);
        let mut out = Vec::new();
        let r = inflate_zlib(&z, &mut out, data.len(), false).unwrap();
        assert!(r.complete);
        assert_eq!(r.consumed, z.len());
        assert_eq!(out, data, "level {}", level.get());
    }

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn roundtrips_all_levels() {
        let text = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(3000);
        let mut mixed = noise(70_000, 7);
        mixed.extend_from_slice(&text);
        mixed.extend(std::iter::repeat_n(0u8, 100_000));
        mixed.extend(noise(5, 9));
        for level in 0..=9 {
            for data in [
                &b""[..],
                b"a",
                b"abcabcabcabc",
                &text,
                &mixed,
                &noise(200_000, 3),
            ] {
                roundtrip(data, Level::new(level));
            }
        }
    }

    #[test]
    fn compresses_repetitive_data() {
        let data = vec![7u8; 1 << 20];
        assert!(compress_zlib(&data, Level::DEFAULT).len() < 2_000);
        let text = b"the quick brown fox jumps over the lazy dog ".repeat(10_000);
        assert!(compress_zlib(&text, Level::DEFAULT).len() < text.len() / 50);
    }

    #[test]
    fn streaming_in_small_pieces_matches_one_shot_decode() {
        let data = noise(50_000, 11).repeat(4);
        let mut c = Compressor::raw(Level::BEST);
        let mut z = Vec::new();
        for chunk in data.chunks(777) {
            c.write(chunk, &mut z);
        }
        c.finish(&mut z);
        let mut out = Vec::new();
        assert_eq!(inflate_raw(&z, &mut out, usize::MAX).unwrap(), z.len());
        assert_eq!(out, data);
    }

    #[test]
    fn huffman_lengths_respect_limit() {
        // Fibonacci frequencies force very deep unrestricted trees.
        let mut f = vec![0u32; 40];
        let (mut a, mut b) = (1u32, 1u32);
        for x in f.iter_mut() {
            *x = a;
            let c = a.saturating_add(b);
            a = b;
            b = c;
        }
        let mut l = vec![0u8; 40];
        build_lengths(&f, 15, &mut l);
        assert!(l.iter().all(|&x| (1..=15).contains(&x)));
        let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(i32::from(x))).sum();
        assert!((kraft - 1.0).abs() < 1e-9);
    }
}
