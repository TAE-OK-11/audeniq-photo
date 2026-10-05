//! CPU features, resolved once per process.
//!
//! zlib-rs asked the CPU at each SIMD call site (several independent
//! caches). Here the whole profile is detected on first use and stored in
//! one atomic word; every query is a single relaxed load and bit test, and
//! features enabled at compile time (`-C target-cpu=...`) fold to constants.
#![allow(dead_code)]

use core::sync::atomic::{AtomicU32, Ordering};

pub struct CpuFeatures;

impl CpuFeatures {
    pub const NONE: usize = 0;
    pub const AVX2: usize = 1;
}

const RESOLVED: u32 = 1 << 31;
const SSE: u32 = 1 << 0;
const SSE42: u32 = 1 << 1;
const AVX2_BMI: u32 = 1 << 2;
const AVX512: u32 = 1 << 3;
const PCLMUL: u32 = 1 << 4;
const NEON: u32 = 1 << 5;
const CRC: u32 = 1 << 6;

static PROFILE: AtomicU32 = AtomicU32::new(0);

#[cold]
fn detect() -> u32 {
    let mut f = RESOLVED;
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        if std::is_x86_feature_detected!("sse") {
            f |= SSE;
        }
        if std::is_x86_feature_detected!("sse4.2") {
            f |= SSE42;
        }
        if std::is_x86_feature_detected!("avx2")
            && std::is_x86_feature_detected!("bmi1")
            && std::is_x86_feature_detected!("bmi2")
        {
            f |= AVX2_BMI;
        }
        if std::is_x86_feature_detected!("avx512f") {
            f |= AVX512;
        }
        if std::is_x86_feature_detected!("pclmulqdq") && std::is_x86_feature_detected!("sse4.1") {
            f |= PCLMUL;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            f |= NEON;
        }
        if std::arch::is_aarch64_feature_detected!("crc") {
            f |= CRC;
        }
    }
    PROFILE.store(f, Ordering::Relaxed);
    f
}

#[inline(always)]
fn has(bit: u32) -> bool {
    let p = PROFILE.load(Ordering::Relaxed);
    let p = if p & RESOLVED == 0 { detect() } else { p };
    p & bit != 0
}

/// Detect now (e.g. at service start) so no request pays for it.
pub fn warm_up() {
    has(SSE);
}

#[inline(always)]
pub fn is_enabled_sse() -> bool {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        #[cfg(target_feature = "sse")]
        return true;
        #[cfg(not(target_feature = "sse"))]
        return has(SSE);
    }
    #[allow(unreachable_code)]
    false
}

#[inline(always)]
pub fn is_enabled_sse42() -> bool {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        #[cfg(target_feature = "sse4.2")]
        return true;
        #[cfg(not(target_feature = "sse4.2"))]
        return has(SSE42);
    }
    #[allow(unreachable_code)]
    false
}

#[inline(always)]
pub fn is_enabled_avx2_and_bmi2() -> bool {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        #[cfg(all(
            target_feature = "avx2",
            target_feature = "bmi1",
            target_feature = "bmi2"
        ))]
        return true;
        #[cfg(not(all(
            target_feature = "avx2",
            target_feature = "bmi1",
            target_feature = "bmi2"
        )))]
        return has(AVX2_BMI);
    }
    #[allow(unreachable_code)]
    false
}

#[inline(always)]
pub fn is_enabled_avx512() -> bool {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    return has(AVX512);
    #[allow(unreachable_code)]
    false
}

#[inline(always)]
pub fn is_enabled_pclmulqdq() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        #[cfg(all(target_feature = "pclmulqdq", target_feature = "sse4.1"))]
        return true;
        #[cfg(not(all(target_feature = "pclmulqdq", target_feature = "sse4.1")))]
        return has(PCLMUL);
    }
    #[allow(unreachable_code)]
    false
}

#[inline(always)]
pub fn is_enabled_neon() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        #[cfg(target_feature = "neon")]
        return true;
        #[cfg(not(target_feature = "neon"))]
        return has(NEON);
    }
    #[allow(unreachable_code)]
    false
}

#[inline(always)]
pub fn is_enabled_crc() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        #[cfg(target_feature = "crc")]
        return true;
        #[cfg(not(target_feature = "crc"))]
        return has(CRC);
    }
    #[allow(unreachable_code)]
    false
}

#[inline(always)]
pub fn is_enabled_lsx() -> bool {
    false
}

#[inline(always)]
pub fn is_enabled_simd128() -> bool {
    false
}
