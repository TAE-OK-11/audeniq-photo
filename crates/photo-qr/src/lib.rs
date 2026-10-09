//! QR code detection for cover-art policy checks: port of quirc's
//! identification pipeline (adaptive threshold, region labelling, finder
//! detection, perspective fitting) with a standard Reed–Solomon decoder.
//!
//! Only *successfully decoded* symbols are counted, matching how the
//! backend used `zbarimg`. Payloads are validated but never returned.
// Only the calls into the AVX2 builds of the pixel loops are `unsafe`.
#![deny(unsafe_code)]

mod decode;
mod identify;
mod rs;
mod tables;

use photo_core::{Deadline, Result};

/// Result of scanning one image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scan {
    /// Finder-pattern triples that looked like a QR grid.
    pub candidates: usize,
    /// Grids that decoded with valid error correction.
    pub decoded: usize,
}

/// Scan an 8-bit grayscale image (row-major, `width * height` bytes).
pub fn scan(gray: &[u8], width: usize, height: usize, deadline: &Deadline) -> Result<Scan> {
    let mut q = identify::Quirc::new(gray, width, height);
    q.identify(deadline)?;
    let mut out = Scan {
        candidates: q.grids.len(),
        decoded: 0,
    };
    for i in 0..q.grids.len() {
        deadline.check()?;
        let code = q.extract(i);
        if decode::decode(&code).is_ok() || decode::decode(&code.flipped()).is_ok() {
            out.decoded += 1;
        }
    }
    Ok(out)
}

/// Rec.601 luma of interleaved 8-bit pixels with `channels` samples; the
/// first three are RGB, a fourth (alpha) is ignored.
pub fn to_gray(pixels: &[u8], channels: usize) -> Vec<u8> {
    match channels {
        1 => pixels.to_vec(),
        2 => pixels.as_chunks::<2>().0.iter().map(|p| p[0]).collect(),
        _ => pixels
            .chunks_exact(channels)
            .map(|p| {
                ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114 + 500)
                    / 1000) as u8
            })
            .collect(),
    }
}
