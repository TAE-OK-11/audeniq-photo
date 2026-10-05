//! Reading metadata from a file path without loading audio payloads.
//!
//! Container metadata is assembled into a compact in-memory copy: sample
//! data chunks (WAV/RF64 `data`, AIFF `SSND`, MP4 `mdat`, FLAC frames,
//! WavPack middle blocks) are skipped with seeks, so a 512 MB master costs a
//! few kilobytes of reads. The compact copy keeps the original layout, so
//! the slice parsers see exactly the structures they would in the file.

use crate::{FileType, Metadata, read};
use photo_core::{Error, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Largest single metadata structure read (a chunk, atom or tag).
const MAX_PART: u64 = 64 * 1024 * 1024;
/// Largest total read for one file.
const MAX_READ: u64 = 128 * 1024 * 1024;
/// Images are read whole up to this size.
const MAX_IMAGE: u64 = 64 * 1024 * 1024;

struct Src {
    f: File,
    len: u64,
    budget: u64,
}

fn io(_: std::io::Error) -> Error {
    Error::Truncated
}

impl Src {
    fn read_at(&mut self, off: u64, n: u64) -> Result<Vec<u8>> {
        if n > MAX_PART || n > self.budget {
            return Err(Error::Limit("metadata structure too large"));
        }
        if off.checked_add(n).is_none_or(|e| e > self.len) {
            return Err(Error::Truncated);
        }
        self.budget -= n;
        self.f.seek(SeekFrom::Start(off)).map_err(io)?;
        let mut v = vec![0; n as usize];
        self.f.read_exact(&mut v).map_err(io)?;
        Ok(v)
    }

    /// Read up to `n` bytes (fewer at end of file).
    fn read_upto(&mut self, off: u64, n: u64) -> Result<Vec<u8>> {
        let n = n.min(self.len.saturating_sub(off));
        self.read_at(off, n)
    }
}

/// Read metadata from a file on disk.
pub fn read_path(path: &Path) -> Result<Metadata> {
    let f = File::open(path).map_err(io)?;
    let len = f.metadata().map_err(io)?.len();
    let mut s = Src { f, len, budget: MAX_READ };
    let head = s.read_upto(0, 64)?;
    let kind = FileType::detect(&head).ok_or(Error::Unsupported("unknown file type"))?;
    let compact = match kind {
        FileType::Jpeg | FileType::Png => {
            if len > MAX_IMAGE {
                return Err(Error::Limit("image too large"));
            }
            s.read_at(0, len)?
        }
        FileType::Wav => chunks(&mut s, 12, false)?,
        FileType::Aiff => chunks(&mut s, 12, true)?,
        FileType::Flac => flac(&mut s)?,
        FileType::Mp4 => mp4(&mut s)?,
        FileType::WavPack => wavpack(&mut s)?,
        FileType::Tta => tta(&mut s)?,
    };
    read(&compact)
}

/// RIFF (little-endian sizes) or IFF/AIFF (big-endian) chunk lists. Sample
/// chunks are kept as headers with a zero size.
fn chunks(s: &mut Src, start: u64, be: bool) -> Result<Vec<u8>> {
    let mut out = s.read_at(0, start)?;
    let rf64 = &out[..4] == b"RF64";
    let mut pos = start;
    let mut rf64_data: Option<u64> = None;
    for _ in 0..4096 {
        if pos + 8 > s.len {
            break;
        }
        let h = s.read_at(pos, 8)?;
        let id = [h[0], h[1], h[2], h[3]];
        let raw = [h[4], h[5], h[6], h[7]];
        let mut size = u64::from(if be { u32::from_be_bytes(raw) } else { u32::from_le_bytes(raw) });
        if rf64 && &id == b"data" && size == 0xFFFF_FFFF {
            size = rf64_data.unwrap_or(s.len - pos - 8);
        }
        let sample = matches!(&id, b"data" | b"SSND") || size > MAX_PART;
        out.extend_from_slice(&id);
        if sample {
            out.extend_from_slice(&[0; 4]);
        } else {
            let body = match s.read_at(pos + 8, size) {
                Ok(b) => b,
                Err(Error::Truncated) => s.read_upto(pos + 8, size)?,
                Err(e) => return Err(e),
            };
            if rf64 && &id == b"ds64" && body.len() >= 16 {
                rf64_data = Some(u64::from_le_bytes(body[8..16].try_into().expect("8")));
            }
            out.extend_from_slice(&raw);
            out.extend_from_slice(&body);
            if size & 1 == 1 {
                out.push(0);
            }
        }
        pos = pos.saturating_add(8 + size + (size & 1));
    }
    Ok(out)
}

fn id3_size(h: &[u8]) -> u64 {
    if h.len() >= 10 && &h[..3] == b"ID3" && h[6..10].iter().all(|&b| b & 0x80 == 0) {
        let n = h[6..10].iter().fold(0u64, |a, &b| a << 7 | u64::from(b));
        10 + n + if h[5] & 0x10 != 0 { 10 } else { 0 }
    } else {
        0
    }
}

fn flac(s: &mut Src) -> Result<Vec<u8>> {
    let head = s.read_upto(0, 10)?;
    let start = id3_size(&head);
    let mut out = s.read_at(0, start + 4)?;
    let mut pos = start + 4;
    for _ in 0..4096 {
        let Ok(h) = s.read_at(pos, 4) else { break };
        let len = u64::from(u32::from_be_bytes([0, h[1], h[2], h[3]]));
        out.extend_from_slice(&h);
        out.extend_from_slice(&s.read_at(pos + 4, len)?);
        pos += 4 + len;
        if h[0] & 0x80 != 0 {
            break;
        }
    }
    Ok(out)
}

fn mp4(s: &mut Src) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    for _ in 0..4096 {
        if pos + 8 > s.len {
            break;
        }
        let h = s.read_at(pos, 8)?;
        let kind = [h[4], h[5], h[6], h[7]];
        let mut size = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let mut hdr = 8;
        if size == 1 {
            size = u64::from_be_bytes(s.read_at(pos + 8, 8)?.try_into().expect("8"));
            hdr = 16;
        } else if size == 0 {
            size = s.len - pos;
        }
        if size < hdr {
            break;
        }
        let keep = matches!(&kind, b"ftyp" | b"moov" | b"meta" | b"udta" | b"uuid") && size <= MAX_PART;
        if keep {
            let body = s.read_at(pos + hdr, size - hdr)?;
            out.extend_from_slice(&((body.len() as u32 + 8).to_be_bytes()));
            out.extend_from_slice(&kind);
            out.extend_from_slice(&body);
        } else {
            out.extend_from_slice(&8u32.to_be_bytes());
            out.extend_from_slice(&kind);
        }
        pos = pos.saturating_add(size);
    }
    Ok(out)
}

/// APE tag and ID3v1 at the end of the file.
fn tail(s: &mut Src) -> Result<Vec<u8>> {
    let mut end = s.len;
    let mut v1 = Vec::new();
    if end >= 128 {
        let t = s.read_at(end - 128, 128)?;
        if &t[..3] == b"TAG" {
            v1 = t;
            end -= 128;
        }
    }
    let mut ape = Vec::new();
    if end >= 32 {
        let f = s.read_at(end - 32, 32)?;
        if &f[..8] == b"APETAGEX" {
            let size = u64::from(u32::from_le_bytes(f[12..16].try_into().expect("4")));
            let flags = u32::from_le_bytes(f[20..24].try_into().expect("4"));
            let header = if flags & 0x8000_0000 != 0 { 32 } else { 0 };
            let total = size + header;
            if total <= end {
                ape = s.read_at(end - total, total)?;
            }
        }
    }
    ape.extend_from_slice(&v1);
    Ok(ape)
}

fn wavpack(s: &mut Src) -> Result<Vec<u8>> {
    let mut first: Option<Vec<u8>> = None;
    let mut last: Option<(u64, u64)> = None;
    let mut pos = 0;
    for _ in 0..(1 << 20) {
        let Ok(h) = s.read_at(pos, 8) else { break };
        if &h[..4] != b"wvpk" {
            break;
        }
        let size = u64::from(u32::from_le_bytes(h[4..8].try_into().expect("4"))) + 8;
        if pos + size > s.len {
            break;
        }
        if first.is_none() {
            first = Some(s.read_at(pos, size)?);
        } else {
            last = Some((pos, size));
        }
        pos += size;
    }
    let mut out = first.ok_or(Error::Invalid("not a WavPack file"))?;
    if let Some((p, n)) = last {
        out.extend_from_slice(&s.read_at(p, n)?);
    }
    out.extend_from_slice(&tail(s)?);
    Ok(out)
}

fn tta(s: &mut Src) -> Result<Vec<u8>> {
    let head = s.read_upto(0, 10)?;
    let start = id3_size(&head);
    let mut out = s.read_at(0, start + 22)?;
    out.extend_from_slice(&tail(s)?);
    Ok(out)
}
