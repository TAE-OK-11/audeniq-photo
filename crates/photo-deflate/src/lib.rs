//! DEFLATE (RFC 1951) and zlib (RFC 1950) for audeniq-photo.
//!
//! The engine (`engine/`) is zlib-rs — a memory-safe Rust port of zlib-ng
//! with SIMD matching, CRC-32 and Adler-32 — merged into this crate (see
//! `ENGINE.md`). The public API adds what the decoders need: hard output
//! limits, a truncating mode for PNG rows, an exact mode for evidence-grade
//! streams, per-thread state reuse and the shared error type.

extern crate alloc;

mod checksum;
mod engine;
mod stream;

pub use checksum::{adler32, adler32_combine, crc32, Adler32, Crc32};
pub use photo_core::{Error, Result};
pub use stream::{
    compress_zlib, inflate_raw, inflate_zlib, inflate_zlib_exact, zlib_trailer, Compressor,
    Inflated, Level, Tuning,
};

/// Detect CPU features now (call at service start) so that no request
/// pays for detection.
pub fn warm_up() {
    engine::cpu_features::warm_up();
}
