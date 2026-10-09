//! Runtime CPU dispatch for the int8 LSTM kernel. The kernel body is the
//! same safe code ([`super::dot_int_rows_body`]) compiled a second time
//! with AVX2 enabled; integer sums are exact, so both give identical
//! results (as Tesseract's generic and SIMD `IntSimdMatrix` kernels do).

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn dot_int_rows_avx2(rows: usize, cols: usize, w: &[i8], scales: &[f32], u: &[i8], v: &mut [f32]) {
    super::dot_int_rows_body(rows, cols, w, scales, u, v);
}

pub(crate) fn dot_int_rows(
    rows: usize,
    cols: usize,
    w: &[i8],
    scales: &[f32],
    u: &[i8],
    v: &mut [f32],
) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2 (checked just above), the only
        // requirement of calling a function compiled for it.
        #[allow(unsafe_code)]
        unsafe {
            dot_int_rows_avx2(rows, cols, w, scales, u, v);
        }
        return;
    }
    super::dot_int_rows_body(rows, cols, w, scales, u, v);
}
