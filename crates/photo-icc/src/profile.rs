//! ICC header and tag table.

use crate::curve::Curve;
use crate::lut::Lut;
use photo_core::{Bytes, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    pub signature: [u8; 4],
    pub offset: u32,
    pub size: u32,
}

/// A parsed ICC profile (the raw bytes are kept for lazy tag decoding).
#[derive(Debug, Clone)]
pub struct Profile {
    data: Vec<u8>,
    pub version: u32,
    pub class: [u8; 4],
    pub color_space: [u8; 4],
    pub pcs: [u8; 4],
    pub tags: Vec<Tag>,
}

const MAX_TAGS: u32 = 256;

impl Profile {
    pub fn parse(data: &[u8]) -> Result<Profile> {
        if data.len() < 132 {
            return Err(Error::Truncated);
        }
        let mut b = Bytes::new(data);
        let size = b.u32_be()? as usize;
        if size < 132 || size > data.len() {
            return Err(Error::Invalid("ICC size"));
        }
        let data = &data[..size];
        b.skip(4)?;
        let version = b.u32_be()?;
        let class: [u8; 4] = b.array()?;
        let color_space: [u8; 4] = b.array()?;
        let pcs: [u8; 4] = b.array()?;
        b.skip(12)?;
        if b.array::<4>()? != *b"acsp" {
            return Err(Error::Invalid("ICC signature"));
        }
        let mut t = Bytes::at(data, 128);
        let count = t.u32_be()?;
        if count > MAX_TAGS {
            return Err(Error::Limit("ICC tag count"));
        }
        let mut tags = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let signature: [u8; 4] = t.array()?;
            let offset = t.u32_be()?;
            let size = t.u32_be()?;
            if u64::from(offset) + u64::from(size) > data.len() as u64 || size < 8 {
                return Err(Error::Invalid("ICC tag bounds"));
            }
            tags.push(Tag { signature, offset, size });
        }
        Ok(Profile { data: data.to_vec(), version, class, color_space, pcs, tags })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn tag(&self, sig: &[u8; 4]) -> Option<&[u8]> {
        self.tags
            .iter()
            .find(|t| &t.signature == sig)
            .map(|t| &self.data[t.offset as usize..(t.offset + t.size) as usize])
    }

    /// Profile ID (MD5) as lowercase hex, as ExifTool prints `ProfileID`.
    pub fn profile_id_hex(&self) -> String {
        self.data[84..100].iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn is_v4(&self) -> bool {
        self.version >= 0x0400_0000
    }

    /// `desc` text: ASCII of a v2 textDescriptionType, or the first
    /// (en-US preferred) record of a v4 multiLocalizedUnicodeType.
    pub fn description(&self) -> Option<String> {
        parse_text(self.tag(b"desc")?)
    }

    pub(crate) fn xyz(&self, sig: &[u8; 4]) -> Result<[f64; 3]> {
        let d = self.tag(sig).ok_or(Error::Invalid("missing XYZ tag"))?;
        if d.len() < 20 || &d[..4] != b"XYZ " {
            return Err(Error::Invalid("XYZ tag type"));
        }
        let f = |o: usize| f64::from(i32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])) / 65536.0;
        Ok([f(8), f(12), f(16)])
    }

    pub(crate) fn curve(&self, sig: &[u8; 4]) -> Result<Curve> {
        Curve::parse(self.tag(sig).ok_or(Error::Invalid("missing TRC tag"))?).map(|(c, _)| c)
    }

    pub(crate) fn lut(&self, sig: &[u8; 4]) -> Option<Result<Lut>> {
        self.tag(sig).map(Lut::parse)
    }

    pub(crate) fn channels(&self) -> Option<usize> {
        match &self.color_space {
            b"GRAY" => Some(1),
            b"RGB " => Some(3),
            b"CMYK" => Some(4),
            _ => None,
        }
    }
}

fn parse_text(d: &[u8]) -> Option<String> {
    let kind = d.get(..4)?;
    match kind {
        b"desc" => {
            let n = u32::from_be_bytes(d.get(8..12)?.try_into().ok()?) as usize;
            let s = d.get(12..12 + n)?;
            let s = s.split(|&b| b == 0).next().unwrap_or(&[]);
            Some(s.iter().map(|&b| char::from(b)).collect())
        }
        b"mluc" => {
            let n = u32::from_be_bytes(d.get(8..12)?.try_into().ok()?) as usize;
            let rec = u32::from_be_bytes(d.get(12..16)?.try_into().ok()?) as usize;
            if rec < 12 || n == 0 {
                return None;
            }
            let mut pick = None;
            for i in 0..n.min(256) {
                let r = d.get(16 + i * rec..16 + i * rec + 12)?;
                let lang = &r[..4];
                let len = u32::from_be_bytes(r[4..8].try_into().ok()?) as usize;
                let off = u32::from_be_bytes(r[8..12].try_into().ok()?) as usize;
                if pick.is_none() || lang == b"enUS" {
                    pick = Some((off, len));
                }
                if lang == b"enUS" {
                    break;
                }
            }
            let (off, len) = pick?;
            let s = d.get(off..off.checked_add(len)?)?;
            let units: Vec<u16> = s.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            let text = String::from_utf16_lossy(&units);
            Some(text.trim_end_matches('\0').to_string())
        }
        b"text" => {
            let s = d.get(8..)?;
            let s = s.split(|&b| b == 0).next().unwrap_or(&[]);
            Some(s.iter().map(|&b| char::from(b)).collect())
        }
        _ => None,
    }
}
