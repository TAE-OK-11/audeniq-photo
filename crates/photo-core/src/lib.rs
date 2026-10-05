//! Shared building blocks: one error type, explicit resource limits and a
//! cooperative deadline. Every decoder in the workspace takes untrusted bytes,
//! so each allocation and each loop over attacker-chosen sizes is bounded here.
#![forbid(unsafe_code)]

use std::fmt;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The input ended before the structure it promised.
    Truncated,
    /// The bytes violate the format.
    Invalid(&'static str),
    /// Valid, but a feature this implementation refuses to process.
    Unsupported(&'static str),
    /// A resource limit (dimensions, pixels, output size) would be exceeded.
    Limit(&'static str),
    /// The cooperative deadline passed.
    Deadline,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("truncated input"),
            Error::Invalid(what) => write!(f, "invalid data: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::Limit(what) => write!(f, "limit exceeded: {what}"),
            Error::Deadline => f.write_str("deadline exceeded"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Upper bounds applied before any pixel buffer is allocated.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_width: u32,
    pub max_height: u32,
    /// Maximum `width * height` of a decoded image.
    pub max_pixels: u64,
    /// Maximum bytes of any single decoder working buffer.
    pub max_alloc: usize,
}

impl Default for Limits {
    fn default() -> Self {
        // Mirrors the backend sanitizer: Pillow's MAX_IMAGE_PIXELS (40 MP)
        // and the 8000 px side limit applied after orientation.
        Limits {
            max_width: 65_535,
            max_height: 65_535,
            max_pixels: 40_000_000,
            max_alloc: 512 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub fn check_dimensions(&self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::Invalid("zero image dimension"));
        }
        if width > self.max_width || height > self.max_height {
            return Err(Error::Limit("image dimensions"));
        }
        if u64::from(width) * u64::from(height) > self.max_pixels {
            return Err(Error::Limit("image pixel count"));
        }
        Ok(())
    }

    /// Size of a buffer of `count` elements of `size` bytes, if allowed.
    pub fn alloc_size(&self, count: u64, size: u64) -> Result<usize> {
        let bytes = count.checked_mul(size).ok_or(Error::Limit("allocation"))?;
        if bytes > self.max_alloc as u64 {
            return Err(Error::Limit("allocation"));
        }
        Ok(bytes as usize)
    }
}

/// Cooperative deadline. Decoders call [`Deadline::check`] between rows or
/// blocks, so a hostile file cannot keep a worker thread busy indefinitely.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    pub const NONE: Deadline = Deadline(None);

    pub fn after(duration: Duration) -> Self {
        Deadline(Instant::now().checked_add(duration))
    }

    pub fn at(instant: Instant) -> Self {
        Deadline(Some(instant))
    }

    #[inline]
    pub fn check(&self) -> Result<()> {
        match self.0 {
            Some(at) if Instant::now() >= at => Err(Error::Deadline),
            _ => Ok(()),
        }
    }
}

/// Pixel layouts shared by the codecs. Samples are interleaved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Gray8,
    GrayAlpha8,
    Rgb8,
    Rgba8,
    Cmyk8,
}

impl PixelFormat {
    pub const fn channels(self) -> usize {
        match self {
            PixelFormat::Gray8 => 1,
            PixelFormat::GrayAlpha8 => 2,
            PixelFormat::Rgb8 => 3,
            PixelFormat::Rgba8 | PixelFormat::Cmyk8 => 4,
        }
    }
}

/// A decoded 8-bit image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

impl Image {
    pub fn stride(&self) -> usize {
        self.width as usize * self.format.channels()
    }

    /// Convert to interleaved RGB8 in place where possible.
    /// Alpha is dropped (as Pillow's `convert("RGB")` does) and CMYK uses
    /// the naive `(255 - c)(255 - k) / 255` transform when no profile applies.
    pub fn into_rgb8(self) -> Image {
        let Image {
            width,
            height,
            format,
            mut data,
        } = self;
        let pixels = width as usize * height as usize;
        match format {
            PixelFormat::Rgb8 => {}
            PixelFormat::Rgba8 | PixelFormat::Cmyk8 => {
                for i in 0..pixels {
                    let s = i * 4;
                    let px = [data[s], data[s + 1], data[s + 2], data[s + 3]];
                    let rgb = if format == PixelFormat::Rgba8 {
                        [px[0], px[1], px[2]]
                    } else {
                        cmyk_to_rgb(px)
                    };
                    data[i * 3..i * 3 + 3].copy_from_slice(&rgb);
                }
                data.truncate(pixels * 3);
            }
            PixelFormat::Gray8 | PixelFormat::GrayAlpha8 => {
                let step = format.channels();
                let mut out = vec![0u8; pixels * 3];
                for (i, px) in out.chunks_exact_mut(3).enumerate() {
                    let g = data[i * step];
                    px.copy_from_slice(&[g, g, g]);
                }
                data = out;
            }
        }
        Image {
            width,
            height,
            format: PixelFormat::Rgb8,
            data,
        }
    }
}

#[inline]
pub fn cmyk_to_rgb([c, m, y, k]: [u8; 4]) -> [u8; 3] {
    let nk = 255 - u32::from(k);
    let f = |v: u8| (nk - div255(u32::from(v) * nk)) as u8;
    [f(c), f(m), f(y)]
}

/// Exact `round(v / 255)` for `v <= 255 * 255`.
#[inline]
pub fn div255(v: u32) -> u32 {
    let t = v + 128;
    (t + (t >> 8)) >> 8
}

/// Bounds-checked big/little-endian readers over a byte slice.
#[derive(Debug, Clone, Copy)]
pub struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bytes { data, pos: 0 }
    }
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Bytes {
            data,
            pos: pos.min(data.len()),
        }
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(Error::Truncated);
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
    pub fn u16_be(&mut self) -> Result<u16> {
        self.array().map(u16::from_be_bytes)
    }
    pub fn u32_be(&mut self) -> Result<u32> {
        self.array().map(u32::from_be_bytes)
    }
    pub fn u64_be(&mut self) -> Result<u64> {
        self.array().map(u64::from_be_bytes)
    }
    pub fn u16_le(&mut self) -> Result<u16> {
        self.array().map(u16::from_le_bytes)
    }
    pub fn u32_le(&mut self) -> Result<u32> {
        self.array().map(u32::from_le_bytes)
    }
    pub fn u64_le(&mut self) -> Result<u64> {
        self.array().map(u64::from_le_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div255_is_exact() {
        for v in 0..=255 * 255 {
            assert_eq!(div255(v), (v as f64 / 255.0).round() as u32, "{v}");
        }
    }

    #[test]
    fn limits_reject_before_allocation() {
        let l = Limits::default();
        assert!(l.check_dimensions(8000, 5000).is_ok());
        assert_eq!(
            l.check_dimensions(8000, 8000),
            Err(Error::Limit("image pixel count"))
        );
        assert!(l.check_dimensions(0, 1).is_err());
        assert!(l.alloc_size(u64::MAX, 2).is_err());
    }

    #[test]
    fn cmyk_matches_pillow_formula() {
        assert_eq!(cmyk_to_rgb([0, 0, 0, 0]), [255, 255, 255]);
        assert_eq!(cmyk_to_rgb([255, 255, 255, 255]), [0, 0, 0]);
        assert_eq!(cmyk_to_rgb([255, 0, 0, 0]), [0, 255, 255]);
    }
}
