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
        return Recip { recip: 1, corr: 0, shift: 0 };
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
    Recip { recip: fq, corr: c, shift: r }
}

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;

#[inline(always)]
fn descale(x: i32, n: i32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

/// `jpeg_fdct_islow` (in place, natural order, output scaled by 8).
fn fdct_islow(d: &mut [i32; 64]) {
    for pass in 0..2 {
        for i in 0..8 {
            let idx = |k: usize| if pass == 0 { i * 8 + k } else { k * 8 + i };
            let p = |k: usize| d[idx(k)];
            let tmp0 = p(0) + p(7);
            let tmp7 = p(0) - p(7);
            let tmp1 = p(1) + p(6);
            let tmp6 = p(1) - p(6);
            let tmp2 = p(2) + p(5);
            let tmp5 = p(2) - p(5);
            let tmp3 = p(3) + p(4);
            let tmp4 = p(3) - p(4);
            let tmp10 = tmp0 + tmp3;
            let tmp13 = tmp0 - tmp3;
            let tmp11 = tmp1 + tmp2;
            let tmp12 = tmp1 - tmp2;
            let (n, s) = if pass == 0 { (CONST_BITS - PASS1_BITS, 0) } else { (CONST_BITS + PASS1_BITS, PASS1_BITS) };
            if pass == 0 {
                d[idx(0)] = (tmp10 + tmp11) << PASS1_BITS;
                d[idx(4)] = (tmp10 - tmp11) << PASS1_BITS;
            } else {
                d[idx(0)] = descale(tmp10 + tmp11, s);
                d[idx(4)] = descale(tmp10 - tmp11, s);
            }
            let z1 = (tmp12 + tmp13) * 4433;
            d[idx(2)] = descale(z1 + tmp13 * 6270, n);
            d[idx(6)] = descale(z1 + tmp12 * -15137, n);
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
            d[idx(7)] = descale(tmp4 + z1 + z3, n);
            d[idx(5)] = descale(tmp5 + z2 + z4, n);
            d[idx(3)] = descale(tmp6 + z2 + z3, n);
            d[idx(1)] = descale(tmp7 + z1 + z4, n);
        }
    }
}

struct HuffCodes {
    code: [u16; 256],
    size: [u8; 256],
}

fn huff_codes(bits: &[u8; 16], vals: &[u8]) -> HuffCodes {
    let mut h = HuffCodes { code: [0; 256], size: [0; 256] };
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
    #[inline(always)]
    fn put(&mut self, bits: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.buf = (self.buf << n) | u64::from(bits & ((1u32 << n) - 1));
        self.n += n;
        while self.n >= 8 {
            let b = (self.buf >> (self.n - 8)) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
            self.n -= 8;
        }
    }
    fn flush(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad);
        }
    }
}

struct Comp {
    qt: [u16; 64],
    recips: [Recip; 64],
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
        Encoder { quality, subsampling }
    }

    pub fn encode(&self, img: &Image) -> Result<Vec<u8>> {
        let gray = match img.format {
            PixelFormat::Gray8 => true,
            PixelFormat::Rgb8 => false,
            _ => return Err(Error::Unsupported("JPEG input must be Gray8 or Rgb8")),
        };
        if img.width == 0 || img.height == 0 || img.width > 65_535 || img.height > 65_535 {
            return Err(Error::Invalid("JPEG dimensions"));
        }
        let (w, h) = (img.width as usize, img.height as usize);
        let sub = if gray { Subsampling::S444 } else { self.subsampling };
        let (hs, vs) = if sub == Subsampling::S420 { (2usize, 2usize) } else { (1, 1) };
        let ncomp = if gray { 1 } else { 3 };
        let lum = quant_table(&STD_LUM, self.quality);
        let chr = quant_table(&STD_CHR, self.quality);

        let mut out = Vec::with_capacity(w * h / 4 + 1024);
        out.extend_from_slice(&[0xFF, 0xD8]);
        // JFIF APP0: version 1.01, aspect 1:1, no thumbnail (jcmarker.c).
        out.extend_from_slice(&[0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
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
            recips: std::array::from_fn(|i| reciprocal(u32::from(qt[i]) * 8)),
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
        let t = rgb_tables();
        for my in 0..mcus_y {
            for ry in 0..mcu_h {
                let sy = (my * mcu_h + ry).min(h - 1);
                let src = &img.data[sy * w * ncomp..(sy + 1) * w * ncomp];
                for x in 0..pw {
                    let sx = x.min(w - 1);
                    if gray {
                        planes[0][ry * pw + x] = src[sx];
                    } else {
                        let (r, g, b) = (src[3 * sx] as usize, src[3 * sx + 1] as usize, src[3 * sx + 2] as usize);
                        planes[0][ry * pw + x] = ((t[r] + t[g + 256] + t[b + 512]) >> 16) as u8;
                        planes[1][ry * pw + x] = ((t[r + 768] + t[g + 1024] + t[b + 1280]) >> 16) as u8;
                        planes[2][ry * pw + x] = ((t[r + 1280] + t[g + 1536] + t[b + 1792]) >> 16) as u8;
                    }
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
                                let s = u32::from(r0[2 * ox]) + u32::from(r0[2 * ox + 1]) + u32::from(r1[2 * ox]) + u32::from(r1[2 * ox + 1]);
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

/// jccolor.c `rgb_ycc_tab`, laid out as 8 consecutive 256-entry tables.
fn rgb_tables() -> &'static [i32; 2048] {
    static T: std::sync::OnceLock<[i32; 2048]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let fix = |x: f64| (x * 65536.0 + 0.5) as i32;
        let one_half = 1 << 15;
        let cbcr_offset = 128 << 16;
        let mut t = [0i32; 2048];
        for i in 0..256 {
            let v = i as i32;
            t[i] = fix(0.29900) * v;
            t[256 + i] = fix(0.58700) * v;
            t[512 + i] = fix(0.11400) * v + one_half;
            t[768 + i] = -fix(0.16874) * v;
            t[1024 + i] = -fix(0.33126) * v;
            t[1280 + i] = fix(0.5) * v + cbcr_offset + one_half - 1;
            t[1536 + i] = -fix(0.41869) * v;
            t[1792 + i] = -fix(0.08131) * v;
        }
        t
    })
}

fn encode_block(bw: &mut BitWriter, c: &mut Comp, plane: &[u8], stride: usize, x0: usize, y0: usize) -> i32 {
    let mut d = [0i32; 64];
    for y in 0..8 {
        for x in 0..8 {
            d[y * 8 + x] = i32::from(plane[(y0 + y) * stride + x0 + x]) - 128;
        }
    }
    fdct_islow(&mut d);
    let mut q = [0i32; 64];
    for i in 0..64 {
        let r = c.recips[i];
        let temp = d[i];
        let mag = temp.unsigned_abs();
        let v = if r.recip == 1 && r.shift == 0 {
            mag
        } else {
            ((mag + r.corr) * r.recip) >> (r.shift)
        };
        q[i] = if temp < 0 { -(v as i32) } else { v as i32 };
    }
    emit(bw, c, &q);
    q[0]
}

fn encode_dc_only(bw: &mut BitWriter, c: &mut Comp, dc: i32) -> i32 {
    let mut q = [0i32; 64];
    q[0] = dc;
    emit(bw, c, &q);
    dc
}

fn emit(bw: &mut BitWriter, c: &mut Comp, q: &[i32; 64]) {
    // DC
    let diff = q[0] - c.pred;
    c.pred = q[0];
    let (nbits, bits) = magnitude(diff);
    bw.put(u32::from(c.dc.code[nbits as usize]), u32::from(c.dc.size[nbits as usize]));
    bw.put(bits, nbits);
    // AC
    let mut run = 0;
    for k in 1..64 {
        let v = q[ZIGZAG[k]];
        if v == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            bw.put(u32::from(c.ac.code[0xF0]), u32::from(c.ac.size[0xF0]));
            run -= 16;
        }
        let (nbits, bits) = magnitude(v);
        let sym = (run << 4) | nbits as usize;
        bw.put(u32::from(c.ac.code[sym]), u32::from(c.ac.size[sym]));
        bw.put(bits, nbits);
        run = 0;
    }
    if run > 0 {
        bw.put(u32::from(c.ac.code[0]), u32::from(c.ac.size[0]));
    }
}

#[inline]
fn magnitude(v: i32) -> (u32, u32) {
    if v == 0 {
        return (0, 0);
    }
    let a = v.unsigned_abs();
    let nbits = 32 - a.leading_zeros();
    let bits = if v < 0 { (v - 1) as u32 } else { v as u32 };
    (nbits, bits & ((1 << nbits) - 1))
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
        assert_eq!(AC_LUM_BITS.iter().map(|&b| b as usize).sum::<usize>(), AC_LUM_VALS.len());
        assert_eq!(AC_CHR_BITS.iter().map(|&b| b as usize).sum::<usize>(), AC_CHR_VALS.len());
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
                let got = if d == 1 { x } else { ((x + r.corr) * r.recip) >> r.shift };
                let want = (x + d / 2) / d;
                assert!(got.abs_diff(want) <= 1, "q={q} x={x}");
            }
        }
    }
}
