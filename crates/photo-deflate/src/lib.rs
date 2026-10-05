//! DEFLATE (RFC 1951) and zlib (RFC 1950) for audeniq-photo.
//!
//! The engine is zlib-rs (vendored as `photo-zlib`): a memory-safe port of
//! zlib-ng with runtime-selected SIMD (AVX2/AVX-512/NEON) for matching,
//! CRC-32 and Adler-32. This crate keeps the workspace's API and adds what
//! the decoders need on top: hard output limits, a truncating mode for PNG
//! rows, an exact mode for evidence-grade streams, and the shared error type.
#![forbid(unsafe_code)]

mod checksum;
mod stream;

pub use checksum::{Adler32, Crc32, adler32, crc32};
pub use photo_core::{Error, Result};
pub use stream::{
    Compressor, Inflated, Level, compress_zlib, inflate_raw, inflate_zlib, inflate_zlib_exact,
};
