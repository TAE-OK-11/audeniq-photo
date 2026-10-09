//! Baseline JPEG encoder: port of libjpeg-turbo's default compression path
//! (jccolor.c, jcsample.c box downsampling, jfdctint.c ISLOW FDCT,
//! jcdctmgr.c reciprocal quantization, standard Huffman tables).
//! Output carries only JFIF, DQT, SOF0, DHT, SOS and EOI.

use crate::ZIGZAG;
use photo_core::{Error, Image, PixelFormat, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsampling {
    /// 4:4:4 (Pillow `subsampling=0`).
    S444,
    /// 4:2:0 (libjpeg default for RGB input).
    S420,
}

const STD_LUM: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const STD_CHR: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

const DC_LUM_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_CHR_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_LUM_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_LUM_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const AC_CHR_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_CHR_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// `jpeg_quality_scaling` + `jpeg_add_quant_table` with force_baseline.
fn quant_table(basic: &[u16; 64], quality: u8) -> [u16; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - q * 2 };
    let mut t = [0u16; 64];
    for i in 0..64 {
        let v = (u32::from(basic[i]) * scale + 50) / 100;
        t[i] = v.clamp(1, 255) as u16;
    }
    t
}

/// libjpeg-turbo `compute_reciprocal` for a 16-bit DCTELEM.
#[derive(Clone, Copy)]
struct Recip {
    recip: u32,
    corr: u32,
    shift: u32,
}

fn reciprocal(divisor: u32) -> Recip {
    if divisor == 1 {
        return Recip {
            recip: 1,
            corr: 0,
            shift: 0,
        };
    }
    let b = 31 - divisor.leading_zeros();
    let mut r = 16 + b;
    let mut fq = (1u32 << r) / divisor;
    let fr = (1u32 << r) % divisor;
    let mut c = divisor / 2;
    if fr == 0 {
        fq >>= 1;
        r -= 1;
    } else if fr <= divisor / 2 {
        c += 1;
    } else {
        fq += 1;
    }
    Recip {
        recip: fq,
        corr: c,
        shift: r,
    }
}

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;

/// `jpeg_fdct_islow` (natural order in and out, output scaled by 8), on
/// eight lanes at a time so it vectorizes; integer results are identical.
#[inline(always)]
fn fdct_islow(d: &[[i32; 8]; 8]) -> [[i32; 8]; 8] {
    /// One 1-D pass over eight lanes: `p[k]` is input sample k of every
    /// lane; returns output k of every lane.
    #[inline(always)]
    fn pass(p: &[[i32; 8]; 8], first: bool) -> [[i32; 8]; 8] {
        let n = if first {
            CONST_BITS - PASS1_BITS
        } else {
            CONST_BITS + PASS1_BITS
        };
        let ds = |x: i32, n: i32| (x + (1 << (n - 1))) >> n;
        let mut o = [[0i32; 8]; 8];
        for l in 0..8 {
            let tmp0 = p[0][l] + p[7][l];
            let tmp7 = p[0][l] - p[7][l];
            let tmp1 = p[1][l] + p[6][l];
            let tmp6 = p[1][l] - p[6][l];
            let tmp2 = p[2][l] + p[5][l];
            let tmp5 = p[2][l] - p[5][l];
            let tmp3 = p[3][l] + p[4][l];
            let tmp4 = p[3][l] - p[4][l];
            let tmp10 = tmp0 + tmp3;
            let tmp13 = tmp0 - tmp3;
            let tmp11 = tmp1 + tmp2;
            let tmp12 = tmp1 - tmp2;
            if first {
                o[0][l] = (tmp10 + tmp11) << PASS1_BITS;
                o[4][l] = (tmp10 - tmp11) << PASS1_BITS;
            } else {
                o[0][l] = ds(tmp10 + tmp11, PASS1_BITS);
                o[4][l] = ds(tmp10 - tmp11, PASS1_BITS);
            }
            let z1 = (tmp12 + tmp13) * 4433;
            o[2][l] = ds(z1 + tmp13 * 6270, n);
            o[6][l] = ds(z1 + tmp12 * -15137, n);
            let z1 = tmp4 + tmp7;
            let z2 = tmp5 + tmp6;
            let z3 = tmp4 + tmp6;
            let z4 = tmp5 + tmp7;
            let z5 = (z3 + z4) * 9633;
            let tmp4 = tmp4 * 2446;
            let tmp5 = tmp5 * 16819;
            let tmp6 = tmp6 * 25172;
            let tmp7 = tmp7 * 12299;
            let z1 = z1 * -7373;
            let z2 = z2 * -20995;
            let z3 = z3 * -16069 + z5;
            let z4 = z4 * -3196 + z5;
            o[7][l] = ds(tmp4 + z1 + z3, n);
            o[5][l] = ds(tmp5 + z2 + z4, n);
            o[3][l] = ds(tmp6 + z2 + z3, n);
            o[1][l] = ds(tmp7 + z1 + z4, n);
        }
        o
    }
    // Pass 1 (rows): lane = row, input k = column k.
    let mut p = [[0i32; 8]; 8];
    for r in 0..8 {
        for k in 0..8 {
            p[k][r] = d[r][k];
        }
    }
    let h = pass(&p, true);
    // Pass 2 (columns): lane = horizontal frequency, input r = row r.
    for r in 0..8 {
        for u in 0..8 {
            p[r][u] = h[u][r];
        }
    }
    pass(&p, false)
}

struct HuffCodes {
    code: [u16; 256],
    size: [u8; 256],
}

fn huff_codes(bits: &[u8; 16], vals: &[u8]) -> HuffCodes {
    let mut h = HuffCodes {
        code: [0; 256],
        size: [0; 256],
    };
    let mut code = 0u16;
    let mut k = 0;
    for l in 1..=16u8 {
        for _ in 0..bits[l as usize - 1] {
            h.code[vals[k] as usize] = code;
            h.size[vals[k] as usize] = l;
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    h
}

struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    n: u32,
}

impl BitWriter {
    /// Append `n <= 32` bits (MSB first) with 0xFF byte stuffing.
    #[inline(always)]
    fn put(&mut self, bits: u32, n: u32) {
        // `n` is never 0 (every symbol has a Huffman code). Callers pass `bits` already confined to `n` bits.
        self.buf = (self.buf << n) | u64::from(bits);
        self.n += n;
        if self.n >= 32 {
            let word = (self.buf >> (self.n - 32)) as u32;
            self.n -= 32;
            let x = !word;
            if x.wrapping_sub(0x0101_0101) & !x & 0x8080_8080 == 0 {
                self.out.extend_from_slice(&word.to_be_bytes());
            } else {
                for b in word.to_be_bytes() {
                    self.out.push(b);
                    if b == 0xFF {
                        self.out.push(0);
                    }
                }
            }
        }
    }

    fn flush(&mut self) {
        // Pad with 1-bits to a byte boundary, then drain whole bytes.
        let pad = (8 - self.n % 8) % 8;
        if pad > 0 {
            self.buf = (self.buf << pad) | ((1u64 << pad) - 1);
            self.n += pad;
        }
        while self.n >= 8 {
            let b = (self.buf >> (self.n - 8)) as u8;
            self.n -= 8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
        }
    }
}

struct Comp {
    qt: [u16; 64],
    recip: [u32; 64],
    corr: [u32; 64],
    shift: [u32; 64],
    dc: HuffCodes,
    ac: HuffCodes,
    pred: i32,
}

/// Baseline JPEG encoder configuration.
#[derive(Debug, Clone, Copy)]
pub struct Encoder {
    pub quality: u8,
    pub subsampling: Subsampling,
}

impl Encoder {
    pub fn new(quality: u8, subsampling: Subsampling) -> Self {
        Encoder {
            quality,
            subsampling,
        }
    }

    pub fn encode(&self, img: &Image) -> Result<Vec<u8>> {
        encode_any(self, img)
    }

    /// The whole encoder, inlined into each instruction-set variant.
    #[inline(always)]
    fn encode_body(&self, img: &Image) -> Result<Vec<u8>> {
        let gray = match img.format {
            PixelFormat::Gray8 => true,
            PixelFormat::Rgb8 => false,
            _ => return Err(Error::Unsupported("JPEG input must be Gray8 or Rgb8")),
        };
        if img.width == 0 || img.height == 0 || img.width > 65_535 || img.height > 65_535 {
            return Err(Error::Invalid("JPEG dimensions"));
        }
        let (w, h) = (img.width as usize, img.height as usize);
        let sub = if gray {
            Subsampling::S444
        } else {
            self.subsampling
        };
        let (hs, vs) = if sub == Subsampling::S420 {
            (2usize, 2usize)
        } else {
            (1, 1)
        };
        let ncomp = if gray { 1 } else { 3 };
        let lum = quant_table(&STD_LUM, self.quality);
        let chr = quant_table(&STD_CHR, self.quality);

        let mut out = Vec::with_capacity(w * h / 4 + 1024);
        out.extend_from_slice(&[0xFF, 0xD8]);
        // JFIF APP0: version 1.01, aspect 1:1, no thumbnail (jcmarker.c).
        out.extend_from_slice(&[
            0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0,
        ]);
        for (i, t) in [lum, chr].iter().enumerate().take(if gray { 1 } else { 2 }) {
            out.extend_from_slice(&[0xFF, 0xDB, 0, 67, i as u8]);
            for k in 0..64 {
                out.push(t[ZIGZAG[k]] as u8);
            }
        }
        let sof_len = 8 + 3 * ncomp;
        out.extend_from_slice(&[0xFF, 0xC0, 0, sof_len as u8, 8]);
        out.extend_from_slice(&(h as u16).to_be_bytes());
        out.extend_from_slice(&(w as u16).to_be_bytes());
        out.push(ncomp as u8);
        out.extend_from_slice(&[1, ((hs << 4) | vs) as u8, 0]);
        if !gray {
            out.extend_from_slice(&[2, 0x11, 1, 3, 0x11, 1]);
        }
        let tables: [(u8, &[u8; 16], &[u8]); 4] = [
            (0x00, &DC_LUM_BITS, &DC_VALS),
            (0x10, &AC_LUM_BITS, &AC_LUM_VALS),
            (0x01, &DC_CHR_BITS, &DC_VALS),
            (0x11, &AC_CHR_BITS, &AC_CHR_VALS),
        ];
        for (class, bits, vals) in tables.iter().take(if gray { 2 } else { 4 }) {
            let len = 2 + 1 + 16 + vals.len();
            out.extend_from_slice(&[0xFF, 0xC4]);
            out.extend_from_slice(&(len as u16).to_be_bytes());
            out.push(*class);
            out.extend_from_slice(*bits);
            out.extend_from_slice(vals);
        }
        let sos_len = 6 + 2 * ncomp;
        out.extend_from_slice(&[0xFF, 0xDA, 0, sos_len as u8, ncomp as u8, 1, 0x00]);
        if !gray {
            out.extend_from_slice(&[2, 0x11, 3, 0x11]);
        }
        out.extend_from_slice(&[0, 63, 0]);

        let mk = |qt: [u16; 64], dcb: &[u8; 16], acb: &[u8; 16], acv: &[u8]| Comp {
            recip: std::array::from_fn(|i| reciprocal(u32::from(qt[i]) * 8).recip),
            corr: std::array::from_fn(|i| reciprocal(u32::from(qt[i]) * 8).corr),
            shift: std::array::from_fn(|i| reciprocal(u32::from(qt[i]) * 8).shift),
            qt,
            dc: huff_codes(dcb, &DC_VALS),
            ac: huff_codes(acb, acv),
            pred: 0,
        };
        let mut comps = vec![mk(lum, &DC_LUM_BITS, &AC_LUM_BITS, &AC_LUM_VALS)];
        if !gray {
            comps.push(mk(chr, &DC_CHR_BITS, &AC_CHR_BITS, &AC_CHR_VALS));
            comps.push(mk(chr, &DC_CHR_BITS, &AC_CHR_BITS, &AC_CHR_VALS));
        }
        let _ = comps[0].qt;
        let mut bw = BitWriter { out, buf: 0, n: 0 };

        let mcu_w = 8 * hs;
        let mcu_h = 8 * vs;
        let mcus_x = w.div_ceil(mcu_w);
        let mcus_y = h.div_ceil(mcu_h);
        let pw = mcus_x * mcu_w;
        // Per MCU row: full-resolution planes (edge-replicated), then
        // downsampled chroma.
        let mut planes = vec![vec![0u8; pw * mcu_h]; ncomp];
        let cw = pw / hs;
        let mut chroma = vec![vec![0u8; cw * 8]; if gray { 0 } else { 2 }];
        for my in 0..mcus_y {
            for ry in 0..mcu_h {
                let sy = (my * mcu_h + ry).min(h - 1);
                let src = &img.data[sy * w * ncomp..(sy + 1) * w * ncomp];
                let row = ry * pw;
                if gray {
                    planes[0][row..row + w].copy_from_slice(src);
                } else {
                    let (p0, rest) = planes.split_at_mut(1);
                    let (p1, p2) = rest.split_at_mut(1);
                    rgb_to_ycc(
                        src,
                        &mut p0[0][row..row + w],
                        &mut p1[0][row..row + w],
                        &mut p2[0][row..row + w],
                    );
                }
                for p in planes.iter_mut() {
                    let edge = p[row + w - 1];
                    p[row + w..row + pw].fill(edge);
                }
            }
            if !gray {
                for c in 0..2 {
                    if hs == 2 {
                        // h2v2_downsample: box average with alternating bias 1, 2.
                        let p = &planes[c + 1];
                        for oy in 0..8 {
                            let r0 = &p[(2 * oy) * pw..(2 * oy + 1) * pw];
                            let r1 = &p[(2 * oy + 1) * pw..(2 * oy + 2) * pw];
                            let mut bias = 1u32;
                            for ox in 0..cw {
                                let s = u32::from(r0[2 * ox])
                                    + u32::from(r0[2 * ox + 1])
                                    + u32::from(r1[2 * ox])
                                    + u32::from(r1[2 * ox + 1]);
                                chroma[c][oy * cw + ox] = ((s + bias) >> 2) as u8;
                                bias ^= 3;
                            }
                        }
                    } else {
                        chroma[c].copy_from_slice(&planes[c + 1][..cw * 8]);
                    }
                }
            }
            for mx in 0..mcus_x {
                // libjpeg (jccoefct.c) codes blocks outside the component's
                // real block grid as "dummy" blocks: zero AC, DC copied from
                // the preceding block (row end: previous block; bottom row:
                // the block just before the dummy row).
                let mut last_dc = 0;
                for v in 0..vs {
                    let row_dc = last_dc;
                    for hh in 0..hs {
                        let (x0, y0) = (mx * mcu_w + hh * 8, v * 8);
                        let real_row = my * vs + v < h.div_ceil(8);
                        let real_col = mx * hs + hh < w.div_ceil(8);
                        last_dc = if !real_row {
                            encode_dc_only(&mut bw, &mut comps[0], row_dc)
                        } else if !real_col {
                            encode_dc_only(&mut bw, &mut comps[0], last_dc)
                        } else {
                            encode_block(&mut bw, &mut comps[0], &planes[0], pw, x0, y0)
                        };
                    }
                }
                if !gray {
                    for c in 0..2 {
                        let (x0, src) = (mx * 8, &chroma[c]);
                        encode_block(&mut bw, &mut comps[c + 1], src, cw, x0, 0);
                    }
                }
            }
        }
        bw.flush();
        let mut out = bw.out;
        out.extend_from_slice(&[0xFF, 0xD9]);
        Ok(out)
    }
}

/// jccolor.c `rgb_ycc_convert` with its table entries expanded into the
/// same integer products (FIX() constants), so it vectorizes.
#[inline(always)]
fn rgb_to_ycc(src: &[u8], y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
    const HALF: i32 = 1 << 15;
    const OFF: i32 = (128 << 16) + HALF - 1;
    for (i, px) in src.chunks_exact(3).enumerate() {
        let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
        y[i] = ((19595 * r + 38470 * g + 7471 * b + HALF) >> 16) as u8;
        cb[i] = ((-11059 * r - 21709 * g + 32768 * b + OFF) >> 16) as u8;
        cr[i] = ((32768 * r - 27439 * g - 5329 * b + OFF) >> 16) as u8;
    }
}

#[inline(always)]
fn encode_block(
    bw: &mut BitWriter,
    c: &mut Comp,
    plane: &[u8],
    stride: usize,
    x0: usize,
    y0: usize,
) -> i32 {
    let mut d = [[0i32; 8]; 8];
    for y in 0..8 {
        let row: &[u8; 8] = plane[(y0 + y) * stride + x0..][..8].try_into().expect("8");
        for x in 0..8 {
            d[y][x] = i32::from(row[x]) - 128;
        }
    }
    let d = fdct_islow(&d);
    // Branch-free reciprocal quantization (vectorizes; divisors are >= 8).
    let mut q = [0i32; 64];
    for i in 0..64 {
        let d = d[i / 8][i % 8];
        let sign = d >> 31;
        let mag = ((d ^ sign) - sign) as u32;
        let v = (((mag + c.corr[i]) * c.recip[i]) >> c.shift[i]) as i32;
        q[i] = (v ^ sign) - sign;
    }
    emit(bw, c, &q);
    q[0]
}

#[inline(always)]
fn encode_dc_only(bw: &mut BitWriter, c: &mut Comp, dc: i32) -> i32 {
    let mut q = [0i32; 64];
    q[0] = dc;
    emit(bw, c, &q);
    dc
}

#[inline(always)]
fn emit(bw: &mut BitWriter, c: &mut Comp, q: &[i32; 64]) {
    // DC
    let diff = q[0] - c.pred;
    c.pred = q[0];
    let (nbits, bits) = magnitude(diff);
    let size = u32::from(c.dc.size[nbits as usize]);
    bw.put(
        (u32::from(c.dc.code[nbits as usize]) << nbits) | bits,
        size + nbits,
    );
    // AC: zigzag order plus a bitmap of non-zero coefficients, so runs of
    // zeros are skipped with one trailing-zeros count (as libjpeg-turbo).
    let mut zz = [0i32; 64];
    let mut mask = 0u64;
    for k in 1..64 {
        let v = q[ZIGZAG[k] & 63];
        zz[k] = v;
        mask |= u64::from(v != 0) << k;
    }
    let mut last = 0u32;
    while mask != 0 {
        let k = mask.trailing_zeros();
        mask &= mask - 1;
        let mut run = k - last - 1;
        last = k;
        while run > 15 {
            bw.put(u32::from(c.ac.code[0xF0]), u32::from(c.ac.size[0xF0]));
            run -= 16;
        }
        let (nbits, bits) = magnitude(zz[k as usize]);
        let sym = ((run << 4) | nbits) as usize;
        let size = u32::from(c.ac.size[sym]);
        bw.put((u32::from(c.ac.code[sym]) << nbits) | bits, size + nbits);
    }
    if last < 63 {
        bw.put(u32::from(c.ac.code[0]), u32::from(c.ac.size[0]));
    }
}

/// JPEG magnitude category and its low bits (one's complement for
/// negatives), branch-free; `(0, 0)` for zero.
#[inline(always)]
fn magnitude(v: i32) -> (u32, u32) {
    let nbits = 32 - v.unsigned_abs().leading_zeros();
    let bits = (v + (v >> 31)) as u32;
    (nbits, bits & ((1u64 << nbits) - 1) as u32)
}

photo_core::multiversion! {
    fn encode_any(e: &Encoder, img: &Image) -> Result<Vec<u8>> = Encoder::encode_body;
}

/// Encode with the given quality and chroma subsampling.
pub fn encode(img: &Image, quality: u8, subsampling: Subsampling) -> Result<Vec<u8>> {
    Encoder::new(quality, subsampling).encode(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_sizes_and_quality_scaling() {
        assert_eq!(
            AC_LUM_BITS.iter().map(|&b| b as usize).sum::<usize>(),
            AC_LUM_VALS.len()
        );
        assert_eq!(
            AC_CHR_BITS.iter().map(|&b| b as usize).sum::<usize>(),
            AC_CHR_VALS.len()
        );
        assert_eq!(quant_table(&STD_LUM, 50), STD_LUM);
        assert_eq!(quant_table(&STD_LUM, 100), [1; 64]);
        assert_eq!(quant_table(&STD_LUM, 95)[0], 2);
    }

    #[test]
    fn reciprocal_matches_rounded_division() {
        for q in 1..=255u32 {
            let d = q * 8;
            let r = reciprocal(d);
            for x in 0..=32767u32 {
                let got = if d == 1 {
                    x
                } else {
                    ((x + r.corr) * r.recip) >> r.shift
                };
                let want = (x + d / 2) / d;
                assert!(got.abs_diff(want) <= 1, "q={q} x={x}");
            }
        }
    }
}
