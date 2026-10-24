//! One library for every image operation the Audeniq backend used to shell
//! out for:
//!
//! | former tool | here |
//! |---|---|
//! | `ffprobe` (cover dimensions) | [`probe`] |
//! | `exiftool` (color properties, provenance tags) | [`color_report`], [`provenance_fields`] |
//! | `zbarimg` (QR count) | [`qr_count`] |
//! | `python3` + Pillow + LittleCMS (`sanitize-upload.py`) | [`sanitize`] |
//! | Poppler `pdfinfo` + `pdftoppm` (document sanitization) | [`pdf::info`], [`pdf::sanitize_pdf`] |
//! | `tesseract -l eng+kor --psm 11 tsv` (artwork text scan) | [`ocr_tsv`] |
//!
//! Every entry point runs in-process on bytes already in memory, inside a
//! panic guard, with explicit pixel/allocation limits and a deadline.
//! All audeniq-photo crates forbid `unsafe` except the DEFLATE engine module
//! of `photo-deflate` (merged from zlib-rs: SIMD kernels and stream buffers)
//! and one self-referential page cache in `photo-pdf-syntax` (from hayro).
#![forbid(unsafe_code)]

mod cover;
mod ocr;
mod orient;
pub mod pdf;
mod probe;
mod report;
pub mod sanitize;

pub use cover::{CoverReport, inspect_cover};
pub use ocr::ocr_tsv;
pub use photo_core::{Deadline, Error as CodecError, Image, Limits, PixelFormat};
pub use probe::{Probe, probe, verify_image};
pub use report::{
    COLOR_FIELDS, PROVENANCE_FIELDS, color_report, metadata, metadata_file, provenance_fields,
};
pub use sanitize::{Kind, decode_image, sanitize};

use std::fmt;

/// Outcome classes the backend distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The bytes are not a supported or valid file (deterministic).
    Invalid(&'static str),
    /// A resource limit or the deadline was hit.
    Limit(&'static str),
    /// A bug in a decoder (caught panic); retrying will not help.
    Internal,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(w) => write!(f, "invalid input: {w}"),
            Error::Limit(w) => write!(f, "limit exceeded: {w}"),
            Error::Internal => f.write_str("internal decoder error"),
        }
    }
}

impl std::error::Error for Error {}

impl From<photo_core::Error> for Error {
    fn from(e: photo_core::Error) -> Self {
        match e {
            photo_core::Error::Limit(w) => Error::Limit(w),
            photo_core::Error::Deadline => Error::Limit("deadline"),
            photo_core::Error::Truncated => Error::Invalid("truncated file"),
            photo_core::Error::Invalid(w) | photo_core::Error::Unsupported(w) => Error::Invalid(w),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Run `f` and turn a panic into [`Error::Internal`] so one malformed file
/// can never take down a worker thread.
pub(crate) fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(Error::Internal),
    }
}

/// Detected raster container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
}

impl Format {
    pub fn detect(data: &[u8]) -> Option<Format> {
        if data.starts_with(&photo_png::SIGNATURE) {
            Some(Format::Png)
        } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Format::Jpeg)
        } else {
            None
        }
    }
}

/// Count decoded QR codes (replacement for `zbarimg -Sqrcode.enable`).
/// The payload is validated but never returned.
pub fn qr_count(data: &[u8], deadline: &Deadline) -> Result<usize> {
    guard(|| {
        let (w, h, gray) = intensity(data, deadline)?;
        Ok(photo_qr::scan_owned(gray, w, h, deadline)?.decoded)
    })
}

/// 8-bit intensity for detectors: JPEG luma straight from the Y plane,
/// otherwise Rec.601 luma of the decoded pixels, computed in place in the
/// decoded buffer (CMYK through its RGB rendering, alpha ignored).
pub(crate) fn intensity(data: &[u8], deadline: &Deadline) -> Result<(usize, usize, Vec<u8>)> {
    let img = match Format::detect(data) {
        Some(Format::Jpeg) => photo_jpeg::decode_luma(data, &Limits::default(), deadline)?.1,
        _ => decode_image(data, &Limits::default(), deadline)?.image,
    };
    Ok((img.width as usize, img.height as usize, luma(img)))
}

/// Rec.601 luma (as [`photo_qr::to_gray`]) of `img`, reusing its buffer.
fn luma(img: Image) -> Vec<u8> {
    let mut gray = img.data;
    match img.format {
        PixelFormat::Gray8 => {}
        PixelFormat::GrayAlpha8 => luma_in_place::<2>(&mut gray, |p| p[0]),
        PixelFormat::Rgb8 => luma_in_place::<3>(&mut gray, |p| photo_qr::luma(p[0], p[1], p[2])),
        PixelFormat::Rgba8 => luma_in_place::<4>(&mut gray, |p| photo_qr::luma(p[0], p[1], p[2])),
        PixelFormat::Cmyk8 => luma_in_place::<4>(&mut gray, |p| {
            let [r, g, b] = photo_core::cmyk_to_rgb(p);
            photo_qr::luma(r, g, b)
        }),
    }
    gray
}

/// Replace `N`-byte pixels by one byte each, front to back (pixel `i` is
/// read before byte `i` is written since `i <= N * i`), and release the
/// tail.
fn luma_in_place<const N: usize>(buf: &mut Vec<u8>, f: impl Fn([u8; N]) -> u8) {
    let n = buf.len() / N;
    for i in 0..n {
        let s = i * N;
        buf[i] = f(buf[s..s + N].try_into().unwrap());
    }
    buf.truncate(n);
    buf.shrink_to_fit();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_matches_copying_conversion() {
        let data: Vec<u8> = (0..4 * 777u32).map(|i| (i * 37 % 256) as u8).collect();
        for format in [
            PixelFormat::Gray8,
            PixelFormat::GrayAlpha8,
            PixelFormat::Rgb8,
            PixelFormat::Rgba8,
            PixelFormat::Cmyk8,
        ] {
            let n = format.channels();
            let img = Image {
                width: 777,
                height: 1,
                format,
                data: data[..777 * n].to_vec(),
            };
            let want = if format == PixelFormat::Cmyk8 {
                let rgb: Vec<u8> = img
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| photo_core::cmyk_to_rgb(*p))
                    .collect();
                photo_qr::to_gray(&rgb, 3)
            } else {
                photo_qr::to_gray(&img.data, n)
            };
            assert_eq!(luma(img), want, "{format:?}");
        }
    }
}
