//! PNG (ISO/IEC 15948) in safe Rust: chunk walking with CRC checks, a
//! decoder for every standard color type, bit depth and Adam7, a streaming
//! metadata-free encoder, and the exact-bytes electronic signature validator.
#![deny(unsafe_code)]

mod decode;
mod encode;
mod signature;

pub use decode::{decode, read_info};
pub use encode::{Encoder, encode};
pub use signature::validate_signature;

use photo_core::{Bytes, Error, Result};

pub const SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

/// Per-chunk and total limits for decompressed text, as in Pillow's
/// `MAX_TEXT_CHUNK` / `MAX_TEXT_MEMORY` but tighter.
pub const MAX_TEXT_CHUNK: usize = 1024 * 1024;
pub const MAX_TEXT_TOTAL: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub kind: [u8; 4],
    pub data: &'a [u8],
    /// Offset of the chunk's length field in the file.
    pub offset: usize,
}

/// Iterate the chunks of a PNG file, verifying each CRC. Iteration stops
/// after IEND; bytes after it are reported by [`ChunkIter::trailing`].
pub struct ChunkIter<'a> {
    bytes: Bytes<'a>,
    done: bool,
}

impl<'a> ChunkIter<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        if data.len() < 8 || data[..8] != SIGNATURE {
            return Err(Error::Invalid("not a PNG file"));
        }
        Ok(ChunkIter {
            bytes: Bytes::at(data, 8),
            done: false,
        })
    }

    /// Bytes following IEND (only meaningful once IEND was returned).
    pub fn trailing(&self) -> usize {
        self.bytes.remaining()
    }
}

impl<'a> Iterator for ChunkIter<'a> {
    type Item = Result<Chunk<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let r = (|| {
            let offset = self.bytes.pos();
            let len = self.bytes.u32_be()?;
            if len > 0x7FFF_FFFF {
                return Err(Error::Invalid("chunk length"));
            }
            let head = self.bytes.take(4)?;
            let kind = [head[0], head[1], head[2], head[3]];
            if !kind.iter().all(u8::is_ascii_alphabetic) {
                return Err(Error::Invalid("chunk type"));
            }
            let data = self.bytes.take(len as usize)?;
            let crc = self.bytes.u32_be()?;
            let mut c = photo_deflate::Crc32::new();
            c.update(&kind);
            c.update(data);
            if c.finish() != crc {
                return Err(Error::Invalid("chunk CRC mismatch"));
            }
            if &kind == b"IEND" {
                self.done = true;
            }
            Ok(Chunk { kind, data, offset })
        })();
        if r.is_err() {
            self.done = true;
        }
        Some(r)
    }
}

/// A textual chunk (tEXt, zTXt or iTXt), decoded to UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub keyword: String,
    pub text: String,
    /// iTXt language tag, if any.
    pub language: Option<String>,
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn inflate_text(data: &[u8], budget: &mut usize) -> Result<Vec<u8>> {
    let limit = MAX_TEXT_CHUNK.min(*budget);
    let mut out = Vec::new();
    photo_deflate::inflate_zlib(data, &mut out, limit, false)?;
    *budget -= out.len();
    Ok(out)
}

/// Decode a tEXt / zTXt / iTXt chunk body. `budget` bounds total
/// decompressed text across a file.
pub fn parse_text(kind: &[u8; 4], data: &[u8], budget: &mut usize) -> Result<Text> {
    let nul = data
        .iter()
        .position(|&b| b == 0)
        .ok_or(Error::Invalid("text keyword"))?;
    if nul == 0 || nul > 79 {
        return Err(Error::Invalid("text keyword length"));
    }
    let keyword = latin1(&data[..nul]);
    let rest = &data[nul + 1..];
    match kind {
        b"tEXt" => {
            if rest.len() > *budget {
                return Err(Error::Limit("text size"));
            }
            *budget -= rest.len();
            Ok(Text {
                keyword,
                text: latin1(rest),
                language: None,
            })
        }
        b"zTXt" => {
            let (&method, body) = rest.split_first().ok_or(Error::Truncated)?;
            if method != 0 {
                return Err(Error::Invalid("zTXt compression method"));
            }
            Ok(Text {
                keyword,
                text: latin1(&inflate_text(body, budget)?),
                language: None,
            })
        }
        b"iTXt" => {
            let mut b = Bytes::new(rest);
            let flag = b.u8()?;
            let method = b.u8()?;
            let rest = b.rest();
            let l = rest.iter().position(|&b| b == 0).ok_or(Error::Truncated)?;
            let language = String::from_utf8_lossy(&rest[..l]).into_owned();
            let rest = &rest[l + 1..];
            let t = rest.iter().position(|&b| b == 0).ok_or(Error::Truncated)?;
            let body = &rest[t + 1..];
            let raw = match (flag, method) {
                (0, _) => {
                    if body.len() > *budget {
                        return Err(Error::Limit("text size"));
                    }
                    *budget -= body.len();
                    body.to_vec()
                }
                (1, 0) => inflate_text(body, budget)?,
                _ => return Err(Error::Invalid("iTXt compression")),
            };
            let text = String::from_utf8(raw).map_err(|_| Error::Invalid("iTXt UTF-8"))?;
            Ok(Text {
                keyword,
                text,
                language: (!language.is_empty()).then_some(language),
            })
        }
        _ => Err(Error::Invalid("not a text chunk")),
    }
}

/// Header and metadata of a PNG, gathered without decoding pixels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: u8,
    pub interlace: bool,
    pub palette: Vec<[u8; 3]>,
    pub transparency: Option<Vec<u8>>,
    /// Decompressed embedded ICC profile (iCCP).
    pub icc_profile: Option<Vec<u8>>,
    pub icc_name: Option<String>,
    pub srgb_intent: Option<u8>,
    /// Raw eXIf payload (a TIFF structure).
    pub exif: Option<Vec<u8>>,
    pub texts: Vec<Text>,
    /// Frame count declared by an APNG acTL chunk.
    pub animation_frames: Option<u32>,
}

impl Info {
    pub fn channels(&self) -> usize {
        match self.color_type {
            0 | 3 => 1,
            4 => 2,
            2 => 3,
            _ => 4,
        }
    }

    pub(crate) fn parse_ihdr(data: &[u8]) -> Result<Info> {
        if data.len() != 13 {
            return Err(Error::Invalid("IHDR length"));
        }
        let mut b = Bytes::new(data);
        let width = b.u32_be()?;
        let height = b.u32_be()?;
        let bit_depth = b.u8()?;
        let color_type = b.u8()?;
        let (compression, filter, interlace) = (b.u8()?, b.u8()?, b.u8()?);
        if width == 0 || height == 0 || width > 0x7FFF_FFFF || height > 0x7FFF_FFFF {
            return Err(Error::Invalid("IHDR dimensions"));
        }
        let ok = match color_type {
            0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
            3 => matches!(bit_depth, 1 | 2 | 4 | 8),
            2 | 4 | 6 => matches!(bit_depth, 8 | 16),
            _ => false,
        };
        if !ok {
            return Err(Error::Invalid("IHDR color type / bit depth"));
        }
        if compression != 0 || filter != 0 || interlace > 1 {
            return Err(Error::Invalid("IHDR methods"));
        }
        Ok(Info {
            width,
            height,
            bit_depth,
            color_type,
            interlace: interlace == 1,
            ..Info::default()
        })
    }
}
