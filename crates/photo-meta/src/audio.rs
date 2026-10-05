//! RIFF/WAVE (INFO, bext, ID3, XMP), AIFF, FLAC (Vorbis comments),
//! WavPack (embedded RIFF header/trailer, APE) and TTA.

use crate::{Metadata, Value, ape, clean, id3, latin1, tag_name, utf8_or_latin1, xmp};
use photo_core::{Error, Result};

const MAX_CHUNKS: usize = 4096;

fn info_name(id: &[u8]) -> String {
    match id {
        b"IARL" => "ArchivalLocation",
        b"IART" => "Artist",
        b"ICMS" => "Commissioned",
        b"ICMT" => "Comment",
        b"ICOP" => "Copyright",
        b"ICRD" => "DateCreated",
        b"IENG" => "Engineer",
        b"IGNR" => "Genre",
        b"IKEY" => "Keywords",
        b"IMED" => "Medium",
        b"INAM" => "Title",
        b"IPRD" => "Product",
        b"ISBJ" => "Subject",
        b"ISFT" => "Software",
        b"ISRC" => "Source",
        b"ISRF" => "SourceForm",
        b"ITCH" => "Technician",
        b"ITRK" | b"IPRT" => "TrackNumber",
        _ => return tag_name(&latin1(id)),
    }
    .to_string()
}

fn zstr(b: &[u8]) -> String {
    clean(&utf8_or_latin1(b.split(|&c| c == 0).next().unwrap_or(&[])))
}

/// Walk RIFF chunks in `d` (the bytes after "RIFF<size>WAVE"). Lenient:
/// stops quietly at a truncated or oversized chunk (e.g. the data chunk).
fn riff_chunks(d: &[u8], m: &mut Metadata) {
    let mut pos = 0;
    for _ in 0..MAX_CHUNKS {
        if pos + 8 > d.len() {
            break;
        }
        let id = &d[pos..pos + 4];
        let size = u32::from_le_bytes(d[pos + 4..pos + 8].try_into().expect("4")) as usize;
        let start = pos + 8;
        let end = match start.checked_add(size) {
            Some(e) if e <= d.len() => e,
            _ => {
                if id != b"data" {
                    m.warn("truncated RIFF chunk");
                }
                break;
            }
        };
        let body = &d[start..end];
        match id {
            b"LIST" if body.len() >= 4 && &body[..4] == b"INFO" => {
                let mut p = 4;
                while p + 8 <= body.len() {
                    let sid = &body[p..p + 4];
                    let ssize = u32::from_le_bytes(body[p + 4..p + 8].try_into().expect("4")) as usize;
                    let Some(v) = body.get(p + 8..(p + 8).saturating_add(ssize)) else { break };
                    m.text("RIFF", &info_name(sid), zstr(v));
                    p += 8 + ssize + (ssize & 1);
                }
            }
            b"bext" if body.len() >= 602 => {
                m.text("RIFF", "Description", zstr(&body[..256]));
                m.text("RIFF", "Originator", zstr(&body[256..288]));
                m.text("RIFF", "OriginatorReference", zstr(&body[288..320]));
                m.text("RIFF", "CodingHistory", zstr(&body[602..]));
            }
            b"id3 " | b"ID3 " => id3::v2(body, m),
            b"_PMX" => xmp::read(body, m),
            b"fmt " if body.len() >= 16 => {
                let bits = u16::from_le_bytes([body[14], body[15]]);
                m.push("RIFF", "BitsPerSample", Value::Int(i64::from(bits)));
            }
            _ => {}
        }
        pos = end + (size & 1);
    }
}

pub(crate) fn riff(d: &[u8], m: &mut Metadata) -> Result<()> {
    if d.len() < 12 || (&d[..4] != b"RIFF" && &d[..4] != b"RF64") || &d[8..12] != b"WAVE" {
        return Err(Error::Invalid("not a WAVE file"));
    }
    riff_chunks(&d[12..], m);
    Ok(())
}

pub(crate) fn aiff(d: &[u8], m: &mut Metadata) -> Result<()> {
    if d.len() < 12 || &d[..4] != b"FORM" {
        return Err(Error::Invalid("not an AIFF file"));
    }
    let mut pos = 12;
    for _ in 0..MAX_CHUNKS {
        if pos + 8 > d.len() {
            break;
        }
        let id = &d[pos..pos + 4];
        let size = u32::from_be_bytes(d[pos + 4..pos + 8].try_into().expect("4")) as usize;
        let start = pos + 8;
        let Some(end) = start.checked_add(size).filter(|&e| e <= d.len()) else {
            if id != b"SSND" {
                m.warn("truncated AIFF chunk");
            }
            break;
        };
        let body = &d[start..end];
        match id {
            b"NAME" => m.text("AIFF", "Name", zstr(body)),
            b"AUTH" => m.text("AIFF", "Author", zstr(body)),
            b"(c) " => m.text("AIFF", "Copyright", zstr(body)),
            b"ANNO" => m.text("AIFF", "Annotation", zstr(body)),
            b"COMT" if body.len() >= 2 => {
                let n = u16::from_be_bytes([body[0], body[1]]) as usize;
                let mut p = 2;
                for _ in 0..n.min(256) {
                    if p + 8 > body.len() {
                        break;
                    }
                    let len = u16::from_be_bytes([body[p + 6], body[p + 7]]) as usize;
                    let Some(t) = body.get(p + 8..p + 8 + len) else { break };
                    m.text("AIFF", "Comment", zstr(t));
                    p += 8 + len + (len & 1);
                }
            }
            b"ID3 " => id3::v2(body, m),
            _ => {}
        }
        pos = end + (size & 1);
    }
    Ok(())
}

fn vorbis_name(key: &str) -> String {
    match key.to_ascii_uppercase().as_str() {
        "TITLE" => "Title",
        "VERSION" => "Version",
        "ALBUM" => "Album",
        "TRACKNUMBER" => "TrackNumber",
        "ARTIST" => "Artist",
        "PERFORMER" => "Performer",
        "COPYRIGHT" => "Copyright",
        "LICENSE" => "License",
        "ORGANIZATION" => "Organization",
        "DESCRIPTION" => "Description",
        "GENRE" => "Genre",
        "DATE" => "Date",
        "LOCATION" => "Location",
        "CONTACT" => "Contact",
        "ISRC" => "ISRC",
        "COMMENT" => "Comment",
        "ENCODER" => "Encoder",
        "ENCODED_USING" => "EncodedUsing",
        "ENCODED_BY" => "EncodedBy",
        "COMPOSER" => "Composer",
        "SOFTWARE" => "Software",
        _ => return tag_name(&key.to_ascii_lowercase()),
    }
    .to_string()
}

pub(crate) fn vorbis_comments(b: &[u8], m: &mut Metadata) {
    let rd = |p: usize| b.get(p..p + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as usize);
    let Some(vlen) = rd(0) else { return };
    let Some(vendor) = b.get(4..4 + vlen) else {
        m.warn("truncated Vorbis comment");
        return;
    };
    m.text("Vorbis", "Vendor", clean(&String::from_utf8_lossy(vendor)));
    let mut p = 4 + vlen;
    let Some(n) = rd(p) else { return };
    p += 4;
    for _ in 0..n.min(4096) {
        let Some(len) = rd(p) else { break };
        let Some(c) = b.get(p + 4..(p + 4).saturating_add(len)) else {
            m.warn("truncated Vorbis comment");
            break;
        };
        p += 4 + len;
        let s = String::from_utf8_lossy(c);
        if let Some((k, v)) = s.split_once('=') {
            m.text("Vorbis", &vorbis_name(k), clean(v));
        }
    }
}

pub(crate) fn flac(d: &[u8], m: &mut Metadata) -> Result<()> {
    let start = id3::skip_header(d);
    if start > 0 {
        id3::v2(d, m);
    }
    if !d[start..].starts_with(b"fLaC") {
        return Err(Error::Invalid("not a FLAC file"));
    }
    let mut pos = start + 4;
    for _ in 0..MAX_CHUNKS {
        if pos + 4 > d.len() {
            break;
        }
        let hdr = d[pos];
        let len = u32::from_be_bytes([0, d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        let Some(body) = d.get(pos + 4..pos + 4 + len) else {
            m.warn("truncated FLAC block");
            break;
        };
        match hdr & 0x7F {
            0 if body.len() >= 18 => {
                let bits = ((u16::from(body[12]) & 1) << 4 | u16::from(body[13]) >> 4) + 1;
                m.push("FLAC", "BitsPerSample", Value::Int(i64::from(bits)));
            }
            4 => vorbis_comments(body, m),
            _ => {}
        }
        pos += 4 + len;
        if hdr & 0x80 != 0 {
            break;
        }
    }
    Ok(())
}

fn tail_tags(d: &[u8], m: &mut Metadata) {
    let v1 = id3::v1(d, m);
    let end = d.len() - v1;
    ape::read(d, end, m);
}

pub(crate) fn wavpack(d: &[u8], m: &mut Metadata) -> Result<()> {
    let mut pos = 0;
    let mut blocks = 0;
    let mut last_trailer: Option<(usize, usize)> = None;
    while pos + 32 <= d.len() && &d[pos..pos + 4] == b"wvpk" && blocks < 1 << 20 {
        let size = u32::from_le_bytes(d[pos + 4..pos + 8].try_into().expect("4")) as usize;
        let Some(end) = (pos + 8).checked_add(size).filter(|&e| e <= d.len()) else { break };
        // Metadata sub-blocks follow the 32-byte header.
        let mut p = pos + 32;
        while p + 2 <= end {
            let id = d[p];
            let (words, hdr) = if id & 0x80 != 0 {
                if p + 4 > end {
                    break;
                }
                (u32::from_le_bytes([d[p + 1], d[p + 2], d[p + 3], 0]) as usize, 4)
            } else {
                (usize::from(d[p + 1]), 2)
            };
            let len = words * 2 - usize::from(id & 0x40 != 0 && words > 0);
            let Some(body) = d.get(p + hdr..p + hdr + len) else { break };
            match id & 0x3F {
                0x21 if blocks == 0 && body.len() >= 12 && &body[..4] == b"RIFF" => riff_chunks(&body[12..], m),
                0x22 => last_trailer = Some((p + hdr, len)),
                _ => {}
            }
            p += hdr + words * 2;
        }
        pos = end;
        blocks += 1;
    }
    if blocks == 0 {
        return Err(Error::Invalid("not a WavPack file"));
    }
    if let Some((s, l)) = last_trailer {
        riff_chunks(&d[s..s + l], m);
    }
    tail_tags(d, m);
    Ok(())
}

pub(crate) fn tta(d: &[u8], m: &mut Metadata) -> Result<()> {
    let start = id3::skip_header(d);
    if start > 0 {
        id3::v2(d, m);
    }
    if !d[start..].starts_with(b"TTA1") {
        return Err(Error::Invalid("not a TTA file"));
    }
    tail_tags(d, m);
    Ok(())
}
