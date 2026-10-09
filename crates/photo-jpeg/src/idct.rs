//! `jpeg_idct_islow` from libjpeg-turbo's jidctint.c, bit-exact.

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;
const FIX_0_298631336: i32 = 2446;
const FIX_0_390180644: i32 = 3196;
const FIX_0_541196100: i32 = 4433;
const FIX_0_765366865: i32 = 6270;
const FIX_0_899976223: i32 = 7373;
const FIX_1_175875602: i32 = 9633;
const FIX_1_501321110: i32 = 12299;
const FIX_1_847759065: i32 = 15137;
const FIX_1_961570560: i32 = 16069;
const FIX_2_053119869: i32 = 16819;
const FIX_2_562915447: i32 = 20995;
const FIX_3_072711026: i32 = 25172;

/// libjpeg's post-IDCT range-limit table, indexed by `(x & 1023)`.
const fn range_table() -> [u8; 1024] {
    let mut t = [0u8; 1024];
    let mut i = 0;
    while i < 1024 {
        t[i] = if i < 128 {
            (i + 128) as u8
        } else if i < 512 {
            255
        } else if i < 896 {
            0
        } else {
            (i - 896) as u8
        };
        i += 1;
    }
    t
}
static RANGE: [u8; 1024] = range_table();

#[inline(always)]
fn descale(x: i32, n: i32) -> i32 {
    x.wrapping_add(1 << (n - 1)) >> n
}

#[inline(always)]
fn limit(x: i32) -> u8 {
    RANGE[(x & 1023) as usize]
}

/// Inverse DCT of one block (natural order, not yet dequantized) into
/// `out` at row stride `stride`. 32-bit wrapping arithmetic, as libjpeg-turbo's
/// SIMD paths use. Each pass works on eight lanes at once so it vectorizes;
/// the zero-column/row shortcuts of jidctint.c are kept as per-lane selects,
/// so results are identical to the scalar algorithm (overflow included).
#[inline(always)]
pub(crate) fn idct_islow(coef: &[i16; 64], quant: &[u16; 64], out: &mut [u8], stride: usize) {
    // DC only (common in smooth areas): every sample is the same.
    if coef[1..].iter().all(|&c| c == 0) {
        let dc = (i32::from(coef[0]) * i32::from(quant[0])).wrapping_shl(PASS1_BITS as u32);
        let v = limit(descale(dc, PASS1_BITS + 3));
        for row in 0..8 {
            out[row * stride..row * stride + 8].fill(v);
        }
        return;
    }
    // Pass 1: lane = column, input k = row k.
    let mut p = [[0i32; 8]; 8];
    let mut zero = [true; 8];
    for r in 0..8 {
        for c in 0..8 {
            p[r][c] = i32::from(coef[r * 8 + c]) * i32::from(quant[r * 8 + c]);
            if r > 0 {
                zero[c] &= coef[r * 8 + c] == 0;
            }
        }
    }
    let o = pass(&p, CONST_BITS - PASS1_BITS);
    let mut ws = [[0i32; 8]; 8];
    for r in 0..8 {
        for c in 0..8 {
            let flat = p[0][c].wrapping_shl(PASS1_BITS as u32);
            ws[r][c] = if zero[c] { flat } else { o[r][c] };
        }
    }
    // Pass 2: lane = row, input k = column k (transpose).
    let mut p = [[0i32; 8]; 8];
    let mut zero = [true; 8];
    for k in 0..8 {
        for r in 0..8 {
            p[k][r] = ws[r][k];
            if k > 0 {
                zero[r] &= ws[r][k] == 0;
            }
        }
    }
    let o = pass(&p, CONST_BITS + PASS1_BITS + 3);
    for row in 0..8 {
        let dst = &mut out[row * stride..row * stride + 8];
        let flat = descale(ws[row][0], PASS1_BITS + 3);
        for k in 0..8 {
            dst[k] = limit(if zero[row] { flat } else { o[k][row] });
        }
    }
}

/// One 1-D IDCT over eight lanes: `p[k]` is input k of every lane; the
/// result is descaled by `n` (output k of every lane).
#[inline(always)]
fn pass(p: &[[i32; 8]; 8], n: i32) -> [[i32; 8]; 8] {
    let mut o = [[0i32; 8]; 8];
    for l in 0..8 {
        let (tmp10, tmp11, tmp12, tmp13) = even(p[0][l], p[2][l], p[4][l], p[6][l]);
        let (t0, t1, t2, t3) = odd(p[7][l], p[5][l], p[3][l], p[1][l]);
        o[0][l] = descale(tmp10.wrapping_add(t3), n);
        o[7][l] = descale(tmp10.wrapping_sub(t3), n);
        o[1][l] = descale(tmp11.wrapping_add(t2), n);
        o[6][l] = descale(tmp11.wrapping_sub(t2), n);
        o[2][l] = descale(tmp12.wrapping_add(t1), n);
        o[5][l] = descale(tmp12.wrapping_sub(t1), n);
        o[3][l] = descale(tmp13.wrapping_add(t0), n);
        o[4][l] = descale(tmp13.wrapping_sub(t0), n);
    }
    o
}

#[inline(always)]
fn even(c0: i32, c2: i32, c4: i32, c6: i32) -> (i32, i32, i32, i32) {
    let z1 = c2.wrapping_add(c6).wrapping_mul(FIX_0_541196100);
    let tmp2 = z1.wrapping_add(c6.wrapping_mul(-FIX_1_847759065));
    let tmp3 = z1.wrapping_add(c2.wrapping_mul(FIX_0_765366865));
    let tmp0 = c0.wrapping_add(c4).wrapping_shl(CONST_BITS as u32);
    let tmp1 = c0.wrapping_sub(c4).wrapping_shl(CONST_BITS as u32);
    (
        tmp0.wrapping_add(tmp3),
        tmp1.wrapping_add(tmp2),
        tmp1.wrapping_sub(tmp2),
        tmp0.wrapping_sub(tmp3),
    )
}

#[inline(always)]
fn odd(tmp0: i32, tmp1: i32, tmp2: i32, tmp3: i32) -> (i32, i32, i32, i32) {
    let z1 = tmp0.wrapping_add(tmp3);
    let z2 = tmp1.wrapping_add(tmp2);
    let z3 = tmp0.wrapping_add(tmp2);
    let z4 = tmp1.wrapping_add(tmp3);
    let z5 = z3.wrapping_add(z4).wrapping_mul(FIX_1_175875602);
    let tmp0 = tmp0.wrapping_mul(FIX_0_298631336);
    let tmp1 = tmp1.wrapping_mul(FIX_2_053119869);
    let tmp2 = tmp2.wrapping_mul(FIX_3_072711026);
    let tmp3 = tmp3.wrapping_mul(FIX_1_501321110);
    let z1 = z1.wrapping_mul(-FIX_0_899976223);
    let z2 = z2.wrapping_mul(-FIX_2_562915447);
    let z3 = z3.wrapping_mul(-FIX_1_961570560).wrapping_add(z5);
    let z4 = z4.wrapping_mul(-FIX_0_390180644).wrapping_add(z5);
    (
        tmp0.wrapping_add(z1).wrapping_add(z3),
        tmp1.wrapping_add(z2).wrapping_add(z4),
        tmp2.wrapping_add(z2).wrapping_add(z3),
        tmp3.wrapping_add(z1).wrapping_add(z4),
    )
}
