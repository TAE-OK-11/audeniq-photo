//! Streaming PNG encoder. Emits only IHDR, IDAT and IEND, so nothing from a
//! source file (text, EXIF, ICC, appended data) can survive re-encoding.

use photo_core::{Error, Image, PixelFormat, Result};
use photo_deflate::{Compressor, Crc32, Level};
use std::io::Write;

const IDAT_CHUNK: usize = 64 * 1024;

fn write_chunk<W: Write>(w: &mut W, kind: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(kind)?;
    w.write_all(data)?;
    let mut c = Crc32::new();
    c.update(kind);
    c.update(data);
    w.write_all(&c.finish().to_be_bytes())
}

fn io(_: std::io::Error) -> Error {
    Error::Invalid("write failed")
}

pub struct Encoder<W: Write> {
    out: W,
    z: Option<Compressor>,
    buf: Vec<u8>,
    bpp: usize,
    stride: usize,
    rows_left: u32,
    prev: Vec<u8>,
    filtered: Vec<u8>,
    scratch: [Vec<u8>; 5],
}

impl<W: Write> Encoder<W> {
    pub fn new(mut out: W, width: u32, height: u32, format: PixelFormat, level: Level) -> Result<Self> {
        let color_type = match format {
            PixelFormat::Gray8 => 0,
            PixelFormat::GrayAlpha8 => 4,
            PixelFormat::Rgb8 => 2,
            PixelFormat::Rgba8 => 6,
            PixelFormat::Cmyk8 => return Err(Error::Unsupported("CMYK PNG")),
        };
        if width == 0 || height == 0 || width > 0x7FFF_FFFF || height > 0x7FFF_FFFF {
            return Err(Error::Invalid("PNG dimensions"));
        }
        out.write_all(&crate::SIGNATURE).map_err(io)?;
        let mut ihdr = [0u8; 13];
        ihdr[..4].copy_from_slice(&width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&height.to_be_bytes());
        ihdr[8] = 8;
        ihdr[9] = color_type;
        write_chunk(&mut out, b"IHDR", &ihdr).map_err(io)?;
        let bpp = format.channels();
        let stride = width as usize * bpp;
        Ok(Encoder {
            out,
            z: Some(Compressor::zlib(level)),
            buf: Vec::with_capacity(IDAT_CHUNK * 2),
            bpp,
            stride,
            rows_left: height,
            prev: Vec::new(),
            filtered: vec![0; stride + 1],
            scratch: std::array::from_fn(|_| vec![0; stride]),
        })
    }

    /// Append one row of `width * channels` bytes.
    pub fn write_row(&mut self, row: &[u8]) -> Result<()> {
        if row.len() != self.stride || self.rows_left == 0 {
            return Err(Error::Invalid("row size"));
        }
        self.rows_left -= 1;
        let prev = (!self.prev.is_empty()).then_some(&self.prev[..]);
        let filter = choose_filter(row, prev, self.bpp, &mut self.scratch);
        self.filtered[0] = filter;
        self.filtered[1..].copy_from_slice(&self.scratch[filter as usize]);
        let z = self.z.as_mut().expect("encoder active");
        z.write(&self.filtered, &mut self.buf);
        self.prev.clear();
        self.prev.extend_from_slice(row);
        self.flush_idat(false)
    }

    fn flush_idat(&mut self, all: bool) -> Result<()> {
        while self.buf.len() >= IDAT_CHUNK || (all && !self.buf.is_empty()) {
            let n = self.buf.len().min(IDAT_CHUNK);
            write_chunk(&mut self.out, b"IDAT", &self.buf[..n]).map_err(io)?;
            self.buf.drain(..n);
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        if self.rows_left != 0 {
            return Err(Error::Invalid("missing rows"));
        }
        let z = self.z.take().expect("encoder active");
        z.finish(&mut self.buf);
        self.flush_idat(true)?;
        write_chunk(&mut self.out, b"IEND", &[]).map_err(io)?;
        self.out.flush().map_err(io)?;
        Ok(self.out)
    }
}

/// Encode a whole image into a PNG byte vector.
pub fn encode(img: &Image, level: Level) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::with_capacity(img.data.len() / 2), img.width, img.height, img.format, level)?;
    for row in img.data.chunks_exact(img.stride()) {
        e.write_row(row)?;
    }
    e.finish()
}

#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = ((p - i16::from(a)).abs(), (p - i16::from(b)).abs(), (p - i16::from(c)).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Filter one row into `out` (without the filter-type byte).
fn apply_filter(filter: u8, row: &[u8], prev: Option<&[u8]>, bpp: usize, out: &mut [u8]) {
    let n = row.len();
    let b = bpp.min(n);
    match (filter, prev) {
        (0, _) => out.copy_from_slice(row),
        (1, _) | (4.., None) => {
            out[..b].copy_from_slice(&row[..b]);
            for i in b..n {
                out[i] = row[i].wrapping_sub(row[i - bpp]);
            }
        }
        (2, None) => out.copy_from_slice(row),
        (2, Some(p)) => {
            for i in 0..n {
                out[i] = row[i].wrapping_sub(p[i]);
            }
        }
        (3, None) => {
            out[..b].copy_from_slice(&row[..b]);
            for i in b..n {
                out[i] = row[i].wrapping_sub(row[i - bpp] >> 1);
            }
        }
        (3, Some(p)) => {
            for i in 0..b {
                out[i] = row[i].wrapping_sub(p[i] >> 1);
            }
            for i in b..n {
                out[i] = row[i].wrapping_sub(((u16::from(row[i - bpp]) + u16::from(p[i])) >> 1) as u8);
            }
        }
        (_, Some(p)) => {
            for i in 0..b {
                out[i] = row[i].wrapping_sub(p[i]);
            }
            for i in b..n {
                out[i] = row[i].wrapping_sub(paeth(row[i - bpp], p[i], p[i - bpp]));
            }
        }
    }
}

#[inline]
fn abs_sum(v: &[u8]) -> u64 {
    v.iter().map(|&x| u64::from((x as i8).unsigned_abs())).sum()
}

/// libpng's minimum-sum-of-absolute-differences heuristic (as Pillow
/// uses). Each candidate is produced into a scratch row once, then the
/// winner is copied; every loop is branch-free and vectorizes.
fn choose_filter(row: &[u8], prev: Option<&[u8]>, bpp: usize, scratch: &mut [Vec<u8>; 5]) -> u8 {
    let mut best = (u64::MAX, 0u8);
    for filter in 0..5u8 {
        if prev.is_none() && (filter == 2 || filter == 4) {
            continue; // identical to None / Sub on the first row
        }
        let buf = &mut scratch[filter as usize];
        apply_filter(filter, row, prev, bpp, buf);
        let sum = abs_sum(buf);
        if sum < best.0 {
            best = (sum, filter);
        }
    }
    best.1
}
