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
/// SIMD paths use; zero shortcuts skip sparse columns and rows.
pub(crate) fn idct_islow(coef: &[i16; 64], quant: &[u16; 64], out: &mut [u8], stride: usize) {
    let mut ws = [0i32; 64];
    for col in 0..8 {
        let c = |r: usize| i32::from(coef[r * 8 + col]) * i32::from(quant[r * 8 + col]);
        if (1..8).all(|r| coef[r * 8 + col] == 0) {
            let dc = c(0).wrapping_shl(PASS1_BITS as u32);
            for r in 0..8 {
                ws[r * 8 + col] = dc;
            }
            continue;
        }
        let (tmp10, tmp11, tmp12, tmp13) = even(c(0), c(2), c(4), c(6));
        let (t0, t1, t2, t3) = odd(c(7), c(5), c(3), c(1));
        let n = CONST_BITS - PASS1_BITS;
        ws[col] = descale(tmp10.wrapping_add(t3), n);
        ws[56 + col] = descale(tmp10.wrapping_sub(t3), n);
        ws[8 + col] = descale(tmp11.wrapping_add(t2), n);
        ws[48 + col] = descale(tmp11.wrapping_sub(t2), n);
        ws[16 + col] = descale(tmp12.wrapping_add(t1), n);
        ws[40 + col] = descale(tmp12.wrapping_sub(t1), n);
        ws[24 + col] = descale(tmp13.wrapping_add(t0), n);
        ws[32 + col] = descale(tmp13.wrapping_sub(t0), n);
    }
    let n = CONST_BITS + PASS1_BITS + 3;
    for row in 0..8 {
        let w = &ws[row * 8..row * 8 + 8];
        let o = &mut out[row * stride..row * stride + 8];
        if w[1..].iter().all(|&v| v == 0) {
            o.fill(limit(descale(w[0], PASS1_BITS + 3)));
            continue;
        }
        let w = |i: usize| w[i];
        let (tmp10, tmp11, tmp12, tmp13) = even(w(0), w(2), w(4), w(6));
        let (t0, t1, t2, t3) = odd(w(7), w(5), w(3), w(1));
        o[0] = limit(descale(tmp10.wrapping_add(t3), n));
        o[7] = limit(descale(tmp10.wrapping_sub(t3), n));
        o[1] = limit(descale(tmp11.wrapping_add(t2), n));
        o[6] = limit(descale(tmp11.wrapping_sub(t2), n));
        o[2] = limit(descale(tmp12.wrapping_add(t1), n));
        o[5] = limit(descale(tmp12.wrapping_sub(t1), n));
        o[3] = limit(descale(tmp13.wrapping_add(t0), n));
        o[4] = limit(descale(tmp13.wrapping_sub(t0), n));
    }
}

#[inline(always)]
fn even(c0: i32, c2: i32, c4: i32, c6: i32) -> (i32, i32, i32, i32) {
    let z1 = c2.wrapping_add(c6).wrapping_mul(FIX_0_541196100);
    let tmp2 = z1.wrapping_add(c6.wrapping_mul(-FIX_1_847759065));
    let tmp3 = z1.wrapping_add(c2.wrapping_mul(FIX_0_765366865));
    let tmp0 = c0.wrapping_add(c4).wrapping_shl(CONST_BITS as u32);
    let tmp1 = c0.wrapping_sub(c4).wrapping_shl(CONST_BITS as u32);
    (tmp0.wrapping_add(tmp3), tmp1.wrapping_add(tmp2), tmp1.wrapping_sub(tmp2), tmp0.wrapping_sub(tmp3))
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
