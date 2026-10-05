//! Electronic signature PNG validation (port of `signature_png` in the
//! backend's former `sanitize-upload.py`). Signatures are stored as exact
//! bytes, so the file itself must be minimal: CRC-checked, a fixed set of
//! static chunks, one complete pixel stream and nothing appended.

use photo_core::{Bytes, Error, Result};

const ALLOWED: [&[u8; 4]; 7] = [
    b"IHDR", b"IDAT", b"IEND", b"pHYs", b"sRGB", b"gAMA", b"cHRM",
];

pub fn validate_signature(data: &[u8]) -> Result<()> {
    if !(45..=45_000).contains(&data.len()) || data[..8] != crate::SIGNATURE {
        return Err(Error::Invalid("invalid signature"));
    }
    let mut pos = 8;
    let mut chunks: Vec<[u8; 4]> = Vec::new();
    let mut encoded = Vec::new();
    let (mut width, mut height, mut colors) = (0u32, 0u32, 0u8);
    while pos < data.len() {
        if data.len() - pos < 12 {
            return Err(Error::Invalid("incomplete signature chunk"));
        }
        let mut b = Bytes::at(data, pos);
        let length = b.u32_be()? as usize;
        let kind: [u8; 4] = b.array()?;
        let end = pos
            .checked_add(length)
            .and_then(|v| v.checked_add(12))
            .ok_or(Error::Truncated)?;
        if end > data.len() || !ALLOWED.contains(&&kind) {
            return Err(Error::Invalid("signature metadata or invalid chunk"));
        }
        let body = &data[pos + 8..pos + 8 + length];
        let crc = u32::from_be_bytes(data[pos + 8 + length..end].try_into().expect("4 bytes"));
        let mut c = photo_deflate::Crc32::new();
        c.update(&kind);
        c.update(body);
        if c.finish() != crc {
            return Err(Error::Invalid("signature CRC mismatch"));
        }
        match &kind {
            b"IHDR" => {
                if !chunks.is_empty() || length != 13 {
                    return Err(Error::Invalid("invalid signature header"));
                }
                let mut h = Bytes::new(body);
                width = h.u32_be()?;
                height = h.u32_be()?;
                let depth = h.u8()?;
                colors = h.u8()?;
                let (compression, filtering, interlace) = (h.u8()?, h.u8()?, h.u8()?);
                if !((1..=2048).contains(&width)
                    && (1..=1024).contains(&height)
                    && u64::from(width) * u64::from(height) <= 1_048_576)
                {
                    return Err(Error::Invalid("signature dimensions exceeded"));
                }
                if depth != 8
                    || ![0, 2, 4, 6].contains(&colors)
                    || compression != 0
                    || filtering != 0
                    || interlace != 0
                {
                    return Err(Error::Invalid("signature pixel format unsupported"));
                }
            }
            b"IDAT" => {
                if chunks.is_empty() || chunks.last() == Some(b"IEND") {
                    return Err(Error::Invalid("invalid signature chunk order"));
                }
                encoded.extend_from_slice(body);
            }
            b"IEND" => {
                if length != 0 || chunks.last() != Some(b"IDAT") || end != data.len() {
                    return Err(Error::Invalid("signature has trailing data"));
                }
            }
            _ => {
                if chunks.is_empty() || chunks.contains(b"IDAT") || chunks.contains(&kind) {
                    return Err(Error::Invalid("invalid signature metadata order"));
                }
                let limit = match &kind {
                    b"pHYs" => 9,
                    b"sRGB" => 1,
                    b"gAMA" => 4,
                    _ => 32,
                };
                if length != limit || (&kind == b"sRGB" && body[0] > 3) {
                    return Err(Error::Invalid("invalid static signature properties"));
                }
            }
        }
        chunks.push(kind);
        pos = end;
    }
    if chunks.last() != Some(b"IEND") {
        return Err(Error::Invalid("incomplete signature"));
    }
    let channels = match colors {
        0 => 1,
        2 => 3,
        4 => 2,
        _ => 4,
    };
    let expected = height as usize * (width as usize * channels + 1);
    photo_deflate::inflate_zlib_exact(&encoded, expected)
        .map_err(|_| Error::Invalid("signature pixel stream mismatch"))?;
    Ok(())
}
