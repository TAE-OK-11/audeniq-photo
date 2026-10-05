//! Marker segment parsing: headers, tables and metadata segments.

use photo_core::{Bytes, Error, Result};

/// A marker segment before the first SOS (APPn, COM, DQT, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    pub marker: u8,
    pub data: &'a [u8],
    pub offset: usize,
}

/// Walk marker segments from SOI up to (excluding) the first SOS or EOI.
/// Returns the segments and the offset of the SOS marker (if any).
pub fn segments(data: &[u8]) -> Result<(Vec<Segment<'_>>, Option<usize>)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(Error::Invalid("not a JPEG file"));
    }
    let mut out = Vec::new();
    let mut pos = 2;
    loop {
        let (marker, at) = next_marker(data, pos)?;
        match marker {
            0xDA => return Ok((out, Some(at))),
            0xD9 => return Ok((out, None)),
            0x01 | 0xD0..=0xD7 => {
                pos = at + 2;
            }
            _ => {
                let mut b = Bytes::at(data, at + 2);
                let len = b.u16_be()? as usize;
                if len < 2 {
                    return Err(Error::Invalid("segment length"));
                }
                let body = b.take(len - 2)?;
                out.push(Segment {
                    marker,
                    data: body,
                    offset: at,
                });
                pos = at + 2 + len;
            }
        }
    }
}

/// Find the next marker at or after `pos`, skipping fill bytes. Garbage
/// between segments is tolerated like libjpeg (which warns and resyncs).
pub(crate) fn next_marker(data: &[u8], mut pos: usize) -> Result<(u8, usize)> {
    loop {
        while pos < data.len() && data[pos] != 0xFF {
            pos += 1;
        }
        let mut p = pos;
        while p < data.len() && data[p] == 0xFF {
            p += 1;
        }
        if p >= data.len() {
            return Err(Error::Truncated);
        }
        if data[p] != 0x00 {
            return Ok((data[p], p - 1));
        }
        pos = p + 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    pub tq: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
    pub marker: u8,
    pub precision: u8,
    pub width: u32,
    pub height: u32,
    pub components: Vec<Component>,
}

impl FrameInfo {
    pub fn progressive(&self) -> bool {
        matches!(self.marker, 0xC2 | 0xC6 | 0xCA | 0xCE)
    }

    pub(crate) fn parse(marker: u8, data: &[u8]) -> Result<FrameInfo> {
        let mut b = Bytes::new(data);
        let precision = b.u8()?;
        let height = u32::from(b.u16_be()?);
        let width = u32::from(b.u16_be()?);
        let n = b.u8()?;
        if n == 0 || n > 4 || data.len() != 6 + 3 * n as usize {
            return Err(Error::Invalid("SOF component count"));
        }
        let mut components = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let id = b.u8()?;
            let hv = b.u8()?;
            let tq = b.u8()?;
            let (h, v) = (hv >> 4, hv & 15);
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
                return Err(Error::Invalid("SOF sampling factors"));
            }
            if components.iter().any(|c: &Component| c.id == id) {
                return Err(Error::Invalid("duplicate component id"));
            }
            components.push(Component { id, h, v, tq });
        }
        Ok(FrameInfo {
            marker,
            precision,
            width,
            height,
            components,
        })
    }
}

/// How 3/4-component data is to be interpreted (libjpeg's
/// `default_decompress_parms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTransform {
    Gray,
    YCbCr,
    Rgb,
    Cmyk,
    Ycck,
}

/// Header-level information gathered from the marker segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub frame: FrameInfo,
    pub jfif: bool,
    /// Adobe APP14 transform flag, if the segment is present.
    pub adobe_transform: Option<u8>,
    pub color: ColorTransform,
    /// Reassembled ICC profile from APP2 `ICC_PROFILE` chunks.
    pub icc_profile: Option<Vec<u8>>,
    /// APP1 EXIF payload (TIFF structure after the `Exif\0\0` header).
    pub exif: Option<Vec<u8>>,
    /// APP1 XMP packet (standard XMP only).
    pub xmp: Option<Vec<u8>>,
    pub comments: Vec<Vec<u8>>,
}

pub(crate) fn color_transform(frame: &FrameInfo, jfif: bool, adobe: Option<u8>) -> ColorTransform {
    let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
    match ids.len() {
        1 => ColorTransform::Gray,
        3 => {
            if jfif {
                ColorTransform::YCbCr
            } else if let Some(t) = adobe {
                if t == 0 {
                    ColorTransform::Rgb
                } else {
                    ColorTransform::YCbCr
                }
            } else if ids == [82, 71, 66] {
                ColorTransform::Rgb
            } else {
                ColorTransform::YCbCr
            }
        }
        4 => match adobe {
            Some(0) | None => ColorTransform::Cmyk,
            _ => ColorTransform::Ycck,
        },
        // Two components: libjpeg treats them as unknown; refuse.
        _ => ColorTransform::Gray,
    }
}

pub(crate) fn assemble_icc(chunks: &mut [(u8, u8, &[u8])]) -> Option<Vec<u8>> {
    if chunks.is_empty() {
        return None;
    }
    let total = chunks[0].1;
    if total == 0 || chunks.len() != total as usize || chunks.iter().any(|c| c.1 != total) {
        return None;
    }
    chunks.sort_by_key(|c| c.0);
    for (i, c) in chunks.iter().enumerate() {
        if c.0 as usize != i + 1 {
            return None;
        }
    }
    Some(chunks.iter().flat_map(|c| c.2.iter().copied()).collect())
}

/// Parse headers and metadata without decoding pixels.
pub fn read_info(data: &[u8]) -> Result<Info> {
    let (segs, _) = segments(data)?;
    let mut frame = None;
    let mut jfif = false;
    let mut adobe = None;
    let mut icc_chunks = Vec::new();
    let (mut exif, mut xmp) = (None, None);
    let mut comments = Vec::new();
    for s in &segs {
        match s.marker {
            0xC0..=0xCF if !matches!(s.marker, 0xC4 | 0xC8 | 0xCC) => {
                if frame.is_some() {
                    return Err(Error::Invalid("multiple SOF markers"));
                }
                frame = Some(FrameInfo::parse(s.marker, s.data)?);
            }
            0xE0 if s.data.starts_with(b"JFIF\0") => jfif = true,
            0xE1 if s.data.starts_with(b"Exif\0") && s.data.len() > 6 => {
                if exif.is_none() {
                    exif = Some(s.data[6..].to_vec());
                }
            }
            0xE1 if s.data.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => {
                if xmp.is_none() {
                    xmp = Some(s.data[29..].to_vec());
                }
            }
            0xE2 if s.data.starts_with(b"ICC_PROFILE\0") && s.data.len() >= 14 => {
                icc_chunks.push((s.data[12], s.data[13], &s.data[14..]));
            }
            0xEE if s.data.starts_with(b"Adobe") && s.data.len() >= 12 => adobe = Some(s.data[11]),
            0xFE => comments.push(s.data.to_vec()),
            _ => {}
        }
    }
    let frame = frame.ok_or(Error::Invalid("missing SOF"))?;
    let color = color_transform(&frame, jfif, adobe);
    Ok(Info {
        icc_profile: assemble_icc(&mut icc_chunks),
        color,
        frame,
        jfif,
        adobe_transform: adobe,
        exif,
        xmp,
        comments,
    })
}
