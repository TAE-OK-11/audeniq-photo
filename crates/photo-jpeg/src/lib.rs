//! JPEG (ITU-T T.81) in safe Rust.
//!
//! The decoder is a port of the libjpeg-turbo pipeline that Pillow uses:
//! `jidctint.c` (ISLOW IDCT), `jdsample.c` fancy upsampling, `jdcolor.c`
//! fixed-point color conversion and `jdphuff.c` progressive decoding, so
//! decoded pixels match Pillow bit for bit on baseline and progressive files.
//! The encoder is a port of the IJG baseline encoder (`jcdctmgr.c`,
//! `jfdctint.c`, standard Huffman tables, `jpeg_quality_scaling`).
#![forbid(unsafe_code)]

mod color;
mod decoder;
mod encoder;
mod huffman;
mod idct;
mod markers;

pub use decoder::{decode, decode_luma};
pub use encoder::{Encoder, Subsampling, encode};
pub use markers::{ColorTransform, Component, FrameInfo, Info, Segment, read_info, segments};

/// Natural-order index of the k-th zigzag coefficient (`jpeg_natural_order`),
/// padded with 63s so corrupt run lengths cannot index out of range.
pub(crate) const ZIGZAG: [usize; 80] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63,
    63, 63, 63, 63, 63, 63, 63, 63,
];
