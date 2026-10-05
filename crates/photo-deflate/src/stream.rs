//! Bounded inflate and streaming deflate over zlib-rs.

use photo_core::{Error, Result};
use zlib_rs::{Deflate, DeflateFlush, Inflate, InflateError, InflateFlush, Status};

/// Compression level, 0 (stored) to 9 (best). Same scale as zlib.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level(u8);

impl Level {
    pub const STORE: Level = Level(0);
    pub const FAST: Level = Level(1);
    pub const DEFAULT: Level = Level(6);
    pub const BEST: Level = Level(9);
    pub fn new(level: u8) -> Self {
        Level(level.min(9))
    }
    pub fn get(self) -> u8 {
        self.0
    }
}

impl Default for Level {
    fn default() -> Self {
        Level::DEFAULT
    }
}

fn inflate_error(e: InflateError) -> Error {
    match e {
        InflateError::NeedDict { .. } => Error::Unsupported("zlib preset dictionary"),
        InflateError::MemError => Error::Limit("inflate memory"),
        _ => Error::Invalid("corrupt deflate stream"),
    }
}

/// Result of a zlib decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inflated {
    /// Bytes of input consumed, including header and checksum.
    pub consumed: usize,
    /// True when the stream ended (checksum verified); false when decoding
    /// stopped at the output limit in truncating mode.
    pub complete: bool,
}

/// Decode into `out` without letting it grow beyond `limit` bytes.
fn run(
    zlib: bool,
    input: &[u8],
    out: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<Inflated> {
    let mut st = Inflate::new(zlib, 15);
    loop {
        let consumed = st.total_in() as usize;
        if out.len() >= limit {
            // Is there more output? Probe with a one-byte buffer.
            let mut probe = [0u8; 1];
            let r = st
                .decompress(&input[consumed..], &mut probe, InflateFlush::NoFlush)
                .map_err(inflate_error)?;
            if st.total_out() as usize > out.len() {
                return if truncate {
                    Ok(Inflated {
                        consumed: st.total_in() as usize,
                        complete: false,
                    })
                } else {
                    Err(Error::Limit("decompressed size"))
                };
            }
            if r == Status::StreamEnd {
                return Ok(Inflated {
                    consumed: st.total_in() as usize,
                    complete: true,
                });
            }
            if st.total_in() as usize == consumed {
                return Err(Error::Truncated);
            }
            continue;
        }
        let room = limit - out.len();
        let chunk = room.min(out.len().max(64 * 1024));
        let old = out.len();
        out.resize(old + chunk, 0);
        let before_out = st.total_out();
        let r = st.decompress(&input[consumed..], &mut out[old..], InflateFlush::NoFlush);
        let produced = (st.total_out() - before_out) as usize;
        out.truncate(old + produced);
        let r = r.map_err(inflate_error)?;
        if r == Status::StreamEnd {
            return Ok(Inflated {
                consumed: st.total_in() as usize,
                complete: true,
            });
        }
        if produced == 0 && st.total_in() as usize == consumed {
            // No progress with output space available: the input ended.
            return Err(Error::Truncated);
        }
    }
}

/// Decode a raw DEFLATE stream, appending to `out`. Returns input bytes consumed.
/// Fails with [`Error::Limit`] if `out` would grow beyond `limit` bytes.
pub fn inflate_raw(input: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize> {
    run(false, input, out, limit, false).map(|r| r.consumed)
}

/// Decode a zlib stream into `out`.
///
/// With `truncate == false` exceeding `limit` is an error. With
/// `truncate == true` decoding stops once `limit` bytes are produced and the
/// result reports `complete: false` (the checksum is not verified then).
pub fn inflate_zlib(
    input: &[u8],
    out: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<Inflated> {
    run(true, input, out, limit, truncate)
}

/// Decode a complete zlib stream that must produce exactly `expected` bytes
/// and be followed by nothing. Used where the exact bytes are evidence.
pub fn inflate_zlib_exact(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let r = inflate_zlib(input, &mut out, expected, false)?;
    if out.len() != expected || r.consumed != input.len() {
        return Err(Error::Invalid("zlib stream size mismatch"));
    }
    Ok(out)
}

/// Streaming compressor producing a raw DEFLATE or a zlib stream.
pub struct Compressor {
    d: Deflate,
    scratch: Vec<u8>,
}

const SCRATCH: usize = 64 * 1024;

impl Compressor {
    /// Raw DEFLATE (no header, no checksum).
    pub fn raw(level: Level) -> Self {
        Self::new(level, false)
    }

    /// zlib-wrapped stream (RFC 1950).
    pub fn zlib(level: Level) -> Self {
        Self::new(level, true)
    }

    fn new(level: Level, zlib: bool) -> Self {
        Compressor {
            d: Deflate::new(i32::from(level.0), zlib, 15),
            scratch: vec![0; SCRATCH],
        }
    }

    fn pump(&mut self, mut input: &[u8], flush: DeflateFlush, out: &mut Vec<u8>) {
        loop {
            let (in0, out0) = (self.d.total_in(), self.d.total_out());
            let r = self
                .d
                .compress(input, &mut self.scratch, flush)
                .expect("deflate stream state is valid");
            let used = (self.d.total_in() - in0) as usize;
            let produced = (self.d.total_out() - out0) as usize;
            out.extend_from_slice(&self.scratch[..produced]);
            input = &input[used..];
            let done = match flush {
                DeflateFlush::Finish => r == Status::StreamEnd,
                _ => input.is_empty() && produced < SCRATCH,
            };
            if done {
                return;
            }
        }
    }

    /// Compress `data`, appending any completed output to `out`.
    pub fn write(&mut self, data: &[u8], out: &mut Vec<u8>) {
        if !data.is_empty() {
            self.pump(data, DeflateFlush::NoFlush, out);
        }
    }

    /// Finish the stream (final block and zlib trailer).
    pub fn finish(mut self, out: &mut Vec<u8>) {
        self.pump(&[], DeflateFlush::Finish, out);
    }
}

/// One-shot zlib compression.
pub fn compress_zlib(data: &[u8], level: Level) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    let mut c = Compressor::zlib(level);
    c.write(data, &mut out);
    c.finish(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn roundtrips_all_levels() {
        let text = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(3000);
        let mut mixed = noise(70_000, 7);
        mixed.extend_from_slice(&text);
        mixed.extend(std::iter::repeat_n(0u8, 100_000));
        for level in 0..=9 {
            for data in [&b""[..], b"a", &text, &mixed, &noise(200_000, 3)] {
                let z = compress_zlib(data, Level::new(level));
                let mut out = Vec::new();
                let r = inflate_zlib(&z, &mut out, data.len(), false).unwrap();
                assert!(
                    r.complete && r.consumed == z.len() && out == data,
                    "level {level}"
                );
            }
        }
    }

    #[test]
    fn streaming_in_small_pieces() {
        let data = noise(50_000, 11).repeat(4);
        let mut c = Compressor::raw(Level::BEST);
        let mut z = Vec::new();
        for chunk in data.chunks(777) {
            c.write(chunk, &mut z);
        }
        c.finish(&mut z);
        let mut out = Vec::new();
        assert_eq!(inflate_raw(&z, &mut out, usize::MAX).unwrap(), z.len());
        assert_eq!(out, data);
    }

    #[test]
    fn limits_truncation_and_exactness() {
        let data = vec![7u8; 1 << 20];
        let z = compress_zlib(&data, Level::DEFAULT);
        assert!(z.len() < 3_000);
        let mut out = Vec::new();
        assert_eq!(
            inflate_zlib(&z, &mut out, data.len() - 1, false),
            Err(Error::Limit("decompressed size"))
        );
        let mut out = Vec::new();
        let r = inflate_zlib(&z, &mut out, 10, true).unwrap();
        assert!(!r.complete && out.len() == 10);
        assert_eq!(inflate_zlib_exact(&z, data.len()).unwrap(), data);
        let mut trailing = z.clone();
        trailing.push(0);
        assert!(inflate_zlib_exact(&trailing, data.len()).is_err());
        for cut in [0, 1, 2, z.len() / 2, z.len() - 1] {
            let mut out = Vec::new();
            assert!(
                inflate_zlib(&z[..cut], &mut out, data.len(), false).is_err(),
                "cut {cut}"
            );
        }
    }
}
