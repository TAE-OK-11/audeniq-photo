//! DEFLATE (RFC 1951) and zlib (RFC 1950) in safe Rust.
//!
//! The decoder follows zlib's `inflate_fast` structure: a 64-bit bit buffer
//! and a single-level lookup table with a canonical slow path for long codes.
//! The encoder is a streaming port of zlib's lazy-matching `deflate_slow`
//! with miniz's length-limited Huffman construction, so memory stays at a
//! fixed ~400 KiB regardless of input size.
#![forbid(unsafe_code)]

mod checksum;
mod deflate;
mod inflate;

pub use checksum::{Adler32, Crc32, adler32, crc32};
pub use deflate::{Compressor, Level, compress_zlib};
pub use inflate::{Inflated, inflate_raw, inflate_zlib, inflate_zlib_exact};
pub use photo_core::{Error, Result};
