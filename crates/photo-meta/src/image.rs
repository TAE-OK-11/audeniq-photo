//! JPEG segment and PNG chunk metadata.

use crate::{Metadata, Value, clean, exif, latin1, tag_name, utf8_or_latin1, xmp};
use photo_core::{Error, Result};

pub(crate) fn jpeg(data: &[u8], m: &mut Metadata) -> Result<()> {
    let (segs, _) = photo_jpeg::segments(data)?;
    let mut icc = Vec::new();
    let mut ext_xmp: Option<(Vec<u8>, Vec<u8>)> = None; // (guid, buffer)
    for s in &segs {
        let d = s.data;
        match s.marker {
            0xC0..=0xCF if !matches!(s.marker, 0xC4 | 0xC8 | 0xCC) => {
                if d.len() >= 6 {
                    m.push(
                        "File",
                        "EncodingProcess",
                        Value::Int(i64::from(s.marker - 0xC0)),
                    );
                    m.push("File", "BitsPerSample", Value::Int(i64::from(d[0])));
                    m.push(
                        "File",
                        "ImageHeight",
                        Value::Int(i64::from(u16::from_be_bytes([d[1], d[2]]))),
                    );
                    m.push(
                        "File",
                        "ImageWidth",
                        Value::Int(i64::from(u16::from_be_bytes([d[3], d[4]]))),
                    );
                    m.push("File", "ColorComponents", Value::Int(i64::from(d[5])));
                }
            }
            0xE0 if d.starts_with(b"JFIF\0") && d.len() >= 7 => {
                m.push(
                    "JFIF",
                    "JFIFVersion",
                    Value::Text(format!("{}.{:02}", d[5], d[6])),
                );
            }
            0xE1 if d.starts_with(b"Exif\0") && d.len() > 6 => exif::read(&d[6..], m),
            0xE1 if d.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => xmp::read(&d[29..], m),
            0xE1 if d.starts_with(b"http://ns.adobe.com/xmp/extension/\0")
                && d.len() >= 35 + 40 =>
            {
                let guid = d[35..67].to_vec();
                let total = u32::from_be_bytes([d[67], d[68], d[69], d[70]]) as usize;
                let off = u32::from_be_bytes([d[71], d[72], d[73], d[74]]) as usize;
                let chunk = &d[75..];
                if total > crate::MAX_TOTAL {
                    m.warn("extended XMP too large");
                    continue;
                }
                let entry = ext_xmp.get_or_insert_with(|| (guid.clone(), vec![0; total]));
                if entry.0 == guid && entry.1.len() == total && off + chunk.len() <= total {
                    entry.1[off..off + chunk.len()].copy_from_slice(chunk);
                }
            }
            0xE2 if d.starts_with(b"ICC_PROFILE\0") && d.len() >= 14 => {
                icc.push((d[12], d[13], &d[14..]))
            }
            0xEE if d.starts_with(b"Adobe") && d.len() >= 12 => {
                m.push("Adobe", "ColorTransform", Value::Int(i64::from(d[11])));
            }
            0xFE => m.text("File", "Comment", clean(&utf8_or_latin1(d))),
            _ => {}
        }
    }
    if let Some((_, buf)) = ext_xmp {
        xmp::read(&buf, m);
    }
    if !icc.is_empty() {
        match assemble(&mut icc) {
            Some(p) => m.icc(&p),
            None => m.warn("incomplete ICC profile chunks"),
        }
    }
    Ok(())
}

fn assemble(chunks: &mut [(u8, u8, &[u8])]) -> Option<Vec<u8>> {
    let total = chunks[0].1;
    if total == 0 || chunks.len() != usize::from(total) {
        return None;
    }
    chunks.sort_by_key(|c| c.0);
    for (i, c) in chunks.iter().enumerate() {
        if usize::from(c.0) != i + 1 || c.1 != total {
            return None;
        }
    }
    Some(chunks.iter().flat_map(|c| c.2.iter().copied()).collect())
}

/// ImageMagick "Raw profile type ..." text: "\nname\n   length\nhex...".
fn raw_profile(text: &str) -> Option<Vec<u8>> {
    let mut lines = text.splitn(4, '\n');
    lines.next()?;
    lines.next()?;
    let len: usize = lines.next()?.trim().parse().ok()?;
    if len > crate::MAX_TOTAL {
        return None;
    }
    let hex = lines.next()?;
    let mut out = Vec::with_capacity(len);
    let mut hi = None;
    for c in hex.chars() {
        let Some(v) = c.to_digit(16) else { continue };
        match hi.take() {
            None => hi = Some(v as u8),
            Some(h) => out.push(h << 4 | v as u8),
        }
        if out.len() == len {
            break;
        }
    }
    (out.len() == len).then_some(out)
}

pub(crate) fn png(data: &[u8], m: &mut Metadata) -> Result<()> {
    if !data.starts_with(&photo_png::SIGNATURE) {
        return Err(Error::Invalid("not a PNG file"));
    }
    let mut pos = 8;
    let mut budget = photo_png::MAX_TEXT_TOTAL;
    let mut first = true;
    while pos + 12 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().expect("4")) as usize;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().expect("4");
        let Some(body) = data.get(pos + 8..(pos + 8).saturating_add(len)) else {
            m.warn("truncated PNG chunk");
            break;
        };
        pos += 12 + len;
        if first && &kind != b"IHDR" {
            return Err(Error::Invalid("PNG without IHDR"));
        }
        first = false;
        match &kind {
            b"IHDR" if body.len() == 13 => {
                let u =
                    |o: usize| i64::from(u32::from_be_bytes(body[o..o + 4].try_into().expect("4")));
                m.push("PNG", "ImageWidth", Value::Int(u(0)));
                m.push("PNG", "ImageHeight", Value::Int(u(4)));
                for (i, n) in [
                    "BitDepth",
                    "ColorType",
                    "Compression",
                    "Filter",
                    "Interlace",
                ]
                .iter()
                .enumerate()
                {
                    m.push("PNG", *n, Value::Int(i64::from(body[8 + i])));
                }
            }
            b"gAMA" if body.len() == 4 => {
                let g = f64::from(u32::from_be_bytes(body.try_into().expect("4")));
                if g > 0.0 {
                    m.push(
                        "PNG",
                        "Gamma",
                        Value::Real((100_000.0 / g * 1e5).round() / 1e5),
                    );
                }
            }
            b"sRGB" if body.len() == 1 => {
                m.push("PNG", "SRGBRendering", Value::Int(i64::from(body[0])))
            }
            b"iCCP" => {
                if let Some(nul) = body.iter().position(|&b| b == 0) {
                    m.text("PNG", "ProfileName", latin1(&body[..nul]));
                    let mut profile = Vec::new();
                    if body.get(nul + 1) == Some(&0)
                        && photo_deflate::inflate_zlib(
                            &body[nul + 2..],
                            &mut profile,
                            4 << 20,
                            false,
                        )
                        .is_ok()
                    {
                        m.icc(&profile);
                    } else {
                        m.warn("invalid iCCP");
                    }
                }
            }
            b"eXIf" => exif::read(body, m),
            b"tEXt" | b"zTXt" | b"iTXt" => match photo_png::parse_text(&kind, body, &mut budget) {
                Ok(t) => text_chunk(&t, m),
                Err(_) => m.warn("invalid PNG text chunk"),
            },
            b"IEND" => break,
            _ => {}
        }
    }
    Ok(())
}

fn text_chunk(t: &photo_png::Text, m: &mut Metadata) {
    let key = t.keyword.as_str();
    match key {
        "XML:com.adobe.xmp" => return xmp::read(t.text.as_bytes(), m),
        _ if key.starts_with("Raw profile type ") => {
            let kind = &key[17..];
            if let Some(bytes) = raw_profile(&t.text) {
                match kind {
                    "exif" | "APP1" => {
                        let body = bytes.strip_prefix(b"Exif\0\0").unwrap_or(&bytes);
                        exif::read(body, m);
                    }
                    "xmp" => xmp::read(&bytes, m),
                    "icc" | "icm" => m.icc(&bytes),
                    _ => {}
                }
            } else {
                m.warn("invalid raw profile");
            }
            return;
        }
        _ => {}
    }
    let mut name = match key {
        "Creation Time" => "CreationTime".to_string(),
        _ => tag_name(key),
    };
    if let Some(lang) = &t.language {
        name = format!("{name}-{lang}");
    }
    m.push("PNG", name, Value::Text(t.text.clone()));
}
