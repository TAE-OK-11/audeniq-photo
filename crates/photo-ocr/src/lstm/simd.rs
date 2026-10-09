//! Runtime CPU dispatch for the int8 LSTM kernel. With AVX2 the shaped
//! weights (see [`super::shape_int`]) go through `vpmaddwd`: one
//! instruction multiplies eight rows' column pairs by the broadcast input
//! pair and adds each pair, so eight row sums grow side by side, as in
//! Tesseract's `IntSimdMatrixAVX2`. Without it, the same exact integer sums
//! come from the portable [`super::dot_int_rows_body`]; results are
//! identical either way.

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn dot_int_rows_avx2(
    rows: usize,
    ni: usize,
    shaped: &[i8],
    bias: &[i32],
    scales: &[f32],
    u: &[i8],
    v: &mut [f32],
) {
    use std::arch::x86_64::*;
    let npairs = ni.div_ceil(2);
    if npairs == 0 {
        return super::dot_int_rows_body(rows, ni, shaped, bias, scales, u, v);
    }
    let u = &u[..ni];
    // Input pairs as packed i16 (u[2k] low, u[2k+1] high; 0 past the end),
    // built once for all row blocks.
    let mut stack = [0i32; 512];
    let mut heap = Vec::new();
    let pairs: &mut [i32] = if npairs <= stack.len() {
        &mut stack[..npairs]
    } else {
        heap.resize(npairs, 0);
        &mut heap
    };
    for (k, p) in pairs.iter_mut().enumerate() {
        let lo = u[2 * k] as i16 as u16 as u32;
        let hi = u.get(2 * k + 1).map_or(0, |&b| b as i16 as u16 as u32);
        *p = (lo | (hi << 16)) as i32;
    }
    let pairs = &*pairs;
    let pair = |k: usize| pairs[k];
    // Sign-extended 16 weight bytes (eight rows x one column pair).
    let load = |b: &[i8]| -> __m256i {
        let b: &[i8; 16] = b.try_into().expect("16");
        let lo = i64::from_le_bytes(std::array::from_fn(|i| b[i] as u8));
        let hi = i64::from_le_bytes(std::array::from_fn(|i| b[8 + i] as u8));
        _mm256_cvtepi8_epi16(_mm_set_epi64x(hi, lo))
    };
    let sums = |acc: __m256i| -> [i32; 8] {
        [
            _mm256_extract_epi32::<0>(acc),
            _mm256_extract_epi32::<1>(acc),
            _mm256_extract_epi32::<2>(acc),
            _mm256_extract_epi32::<3>(acc),
            _mm256_extract_epi32::<4>(acc),
            _mm256_extract_epi32::<5>(acc),
            _mm256_extract_epi32::<6>(acc),
            _mm256_extract_epi32::<7>(acc),
        ]
    };
    let mut store = |blk: usize, acc: __m256i| {
        for (r, &sum) in sums(acc).iter().enumerate() {
            let row = blk * 8 + r;
            if row < rows {
                v[row] = (sum + bias[row]) as f32 * scales[row];
            }
        }
    };
    let block = npairs * 16;
    let blocks = shaped.len() / block;
    let mut blk = 0;
    while blk < blocks {
        let wa = &shaped[blk * block..(blk + 1) * block];
        let (mut acc0, mut acc1) = (_mm256_setzero_si256(), _mm256_setzero_si256());
        let mut k = 0;
        while k + 1 < npairs {
            acc0 = _mm256_add_epi32(
                acc0,
                _mm256_madd_epi16(load(&wa[k * 16..k * 16 + 16]), _mm256_set1_epi32(pair(k))),
            );
            acc1 = _mm256_add_epi32(
                acc1,
                _mm256_madd_epi16(
                    load(&wa[k * 16 + 16..k * 16 + 32]),
                    _mm256_set1_epi32(pair(k + 1)),
                ),
            );
            k += 2;
        }
        if k < npairs {
            acc0 = _mm256_add_epi32(
                acc0,
                _mm256_madd_epi16(load(&wa[k * 16..k * 16 + 16]), _mm256_set1_epi32(pair(k))),
            );
        }
        store(blk, _mm256_add_epi32(acc0, acc1));
        blk += 1;
    }
}

pub(crate) fn dot_int_rows(
    rows: usize,
    ni: usize,
    shaped: &[i8],
    bias: &[i32],
    scales: &[f32],
    u: &[i8],
    v: &mut [f32],
) {
    #[cfg(target_arch = "x86_64")]
    if photo_core::cpu::x86_v3() {
        // SAFETY: the CPU supports AVX2 (x86-64-v3, checked just above),
        // the only requirement of calling a function compiled for it.
        #[allow(unsafe_code)]
        return unsafe { dot_int_rows_avx2(rows, ni, shaped, bias, scales, u, v) };
    }
    super::dot_int_rows_body(rows, ni, shaped, bias, scales, u, v);
}

/// Whether int8 weights are shaped for [`dot_int_rows_vnni`].
pub(crate) fn vnni() -> bool {
    #[cfg(target_arch = "x86_64")]
    return photo_core::cpu::x86_vnni();
    #[cfg(not(target_arch = "x86_64"))]
    false
}

/// AVX-VNNI kernel over weights shaped in groups of four columns. The
/// inputs go in as unsigned bytes `u + 128` (`vpdpbusd` multiplies u8 by
/// i8 and adds four products per lane without saturating); the shaped
/// bias already subtracts `128 * sum(row)`, so the integer sums are
/// exactly those of the other kernels.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,avxvnni")]
fn dot_vnni(
    rows: usize,
    ni: usize,
    shaped: &[i8],
    bias: &[i32],
    scales: &[f32],
    u: &[i8],
    v: &mut [f32],
) {
    use std::arch::x86_64::*;
    let nquads = ni.div_ceil(4);
    let u = &u[..ni];
    let mut stack = [0i32; 256];
    let mut heap = Vec::new();
    let quads: &mut [i32] = if nquads <= stack.len() {
        &mut stack[..nquads]
    } else {
        heap.resize(nquads, 0);
        &mut heap
    };
    for (k, q) in quads.iter_mut().enumerate() {
        // Past the last column the weights are 0, so any byte will do.
        let b = |j: usize| u.get(4 * k + j).map_or(128u8, |&x| (x as u8) ^ 0x80);
        *q = i32::from_le_bytes([b(0), b(1), b(2), b(3)]);
    }
    let quads = &*quads;
    let load = |b: &[i8]| -> __m256i {
        let b: &[i8; 32] = b.try_into().expect("32");
        let q = |o: usize| i64::from_le_bytes(std::array::from_fn(|i| b[o + i] as u8));
        _mm256_set_epi64x(q(24), q(16), q(8), q(0))
    };
    let block = nquads * 32;
    if block == 0 {
        for row in 0..rows {
            v[row] = bias[row] as f32 * scales[row];
        }
        return;
    }
    for (blk, wb) in shaped.chunks_exact(block).enumerate() {
        let (mut acc0, mut acc1) = (_mm256_setzero_si256(), _mm256_setzero_si256());
        let mut k = 0;
        while k + 1 < nquads {
            acc0 = _mm256_dpbusd_avx_epi32(
                acc0,
                _mm256_set1_epi32(quads[k]),
                load(&wb[k * 32..k * 32 + 32]),
            );
            acc1 = _mm256_dpbusd_avx_epi32(
                acc1,
                _mm256_set1_epi32(quads[k + 1]),
                load(&wb[k * 32 + 32..k * 32 + 64]),
            );
            k += 2;
        }
        if k < nquads {
            acc0 = _mm256_dpbusd_avx_epi32(
                acc0,
                _mm256_set1_epi32(quads[k]),
                load(&wb[k * 32..k * 32 + 32]),
            );
        }
        let acc = _mm256_add_epi32(acc0, acc1);
        let sums = [
            _mm256_extract_epi32::<0>(acc),
            _mm256_extract_epi32::<1>(acc),
            _mm256_extract_epi32::<2>(acc),
            _mm256_extract_epi32::<3>(acc),
            _mm256_extract_epi32::<4>(acc),
            _mm256_extract_epi32::<5>(acc),
            _mm256_extract_epi32::<6>(acc),
            _mm256_extract_epi32::<7>(acc),
        ];
        for (r, &sum) in sums.iter().enumerate() {
            let row = blk * 8 + r;
            if row < rows {
                v[row] = (sum + bias[row]) as f32 * scales[row];
            }
        }
    }
}

/// [`dot_vnni`] (weights must be shaped for it: see [`vnni`]).
pub(crate) fn dot_int_rows_vnni(
    rows: usize,
    ni: usize,
    shaped: &[i8],
    bias: &[i32],
    scales: &[f32],
    u: &[i8],
    v: &mut [f32],
) {
    #[cfg(target_arch = "x86_64")]
    {
        assert!(vnni(), "VNNI-shaped weights on a CPU without AVX-VNNI");
        // SAFETY: AVX2 and AVX-VNNI are present (asserted just above), the
        // only requirement of calling a function compiled for them.
        #[allow(unsafe_code)]
        unsafe {
            dot_vnni(rows, ni, shaped, bias, scales, u, v)
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    unreachable!("VNNI layout is only chosen on x86-64");
}
