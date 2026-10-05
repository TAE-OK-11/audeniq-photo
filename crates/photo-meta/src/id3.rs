//! ID3v1 and ID3v2.2/2.3/2.4 (ExifTool ID3.pm names, groups `ID3v2_N`).

use crate::{Metadata, clean, latin1, tag_name};

/// Size of a leading ID3v2 tag (0 if none).
pub(crate) fn skip_header(d: &[u8]) -> usize {
    if d.len() >= 10
        && &d[..3] == b"ID3"
        && d[3] < 5
        && let Some(size) = syncsafe(&d[6..10])
    {
        let footer = if d[5] & 0x10 != 0 { 10 } else { 0 };
        return (10 + size as usize + footer).min(d.len());
    }
    0
}

fn syncsafe(b: &[u8]) -> Option<u32> {
    if b.iter().any(|&x| x & 0x80 != 0) {
        return None;
    }
    Some(b.iter().fold(0u32, |acc, &x| acc << 7 | u32::from(x)))
}

fn unsync(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut i = 0;
    while i < d.len() {
        out.push(d[i]);
        if d[i] == 0xFF && d.get(i + 1) == Some(&0) {
            i += 1;
        }
        i += 1;
    }
    out
}

fn decode_text(enc: u8, b: &[u8]) -> String {
    match enc {
        1 | 2 => {
            let (le, body) = match b {
                [0xFF, 0xFE, r @ ..] => (true, r),
                [0xFE, 0xFF, r @ ..] => (false, r),
                _ => (enc == 1, b),
            };
            let units: Vec<u16> = body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| {
                    if le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        3 => String::from_utf8_lossy(b).into_owned(),
        _ => latin1(b),
    }
}

/// Split at the encoding-appropriate terminator.
fn split_term(enc: u8, b: &[u8]) -> (&[u8], &[u8]) {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == 0 && b[i + 1] == 0 {
                return (&b[..i], &b[i + 2..]);
            }
            i += 2;
        }
        (b, &[])
    } else {
        match b.iter().position(|&x| x == 0) {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => (b, &[]),
        }
    }
}

fn frame_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "TIT2" | "TT2" => "Title",
        "TPE1" | "TP1" => "Artist",
        "TPE2" | "TP2" => "Band",
        "TALB" | "TAL" => "Album",
        "TCOM" | "TCM" => "Composer",
        "TCON" | "TCO" => "Genre",
        "TYER" | "TYE" => "Year",
        "TDRC" => "RecordingTime",
        "TRCK" | "TRK" => "Track",
        "TCOP" | "TCR" => "Copyright",
        "TPUB" | "TPB" => "Publisher",
        "TSRC" | "TRC" => "ISRC",
        "TENC" | "TEN" => "EncodedBy",
        "TSSE" | "TSS" => "EncoderSettings",
        "TIT3" | "TT3" => "Subtitle",
        "TEXT" | "TXT" => "Lyricist",
        "TOWN" => "FileOwner",
        _ => return None,
    })
}

/// Parse an ID3v2 tag at the start of `d`.
pub(crate) fn v2(d: &[u8], m: &mut Metadata) {
    let total = skip_header(d);
    if total < 10 {
        return;
    }
    let (major, flags) = (d[3], d[5]);
    let group = format!("ID3v2_{major}");
    let mut body: Vec<u8> = d[10..total].to_vec();
    if flags & 0x80 != 0 && major < 4 {
        body = unsync(&body);
    }
    let mut pos = 0;
    if flags & 0x40 != 0 {
        // Extended header.
        let size = if major == 4 {
            syncsafe(body.get(0..4).unwrap_or(&[0x80])).unwrap_or(0) as usize
        } else {
            body.get(0..4).map_or(0, |b| {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize + 4
            })
        };
        pos = size;
    }
    let (id_len, hdr) = if major == 2 { (3, 6) } else { (4, 10) };
    let mut count = 0;
    while pos + hdr <= body.len() && count < 2048 {
        count += 1;
        let id_bytes = &body[pos..pos + id_len];
        if id_bytes[0] == 0 {
            break;
        }
        if !id_bytes
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            m.warn("invalid ID3 frame id");
            break;
        }
        let id = latin1(id_bytes);
        let size = match major {
            2 => u32::from_be_bytes([0, body[pos + 3], body[pos + 4], body[pos + 5]]) as usize,
            3 => u32::from_be_bytes(body[pos + 4..pos + 8].try_into().expect("4")) as usize,
            _ => syncsafe(&body[pos + 4..pos + 8]).unwrap_or(u32::MAX) as usize,
        };
        let fflags = if major == 2 {
            0
        } else {
            u16::from_be_bytes([body[pos + 8], body[pos + 9]])
        };
        let start = pos + hdr;
        let Some(end) = start.checked_add(size).filter(|&e| e <= body.len()) else {
            m.warn("truncated ID3 frame");
            break;
        };
        pos = end;
        let mut f: Vec<u8> = body[start..end].to_vec();
        if major == 4 {
            if fflags & 0x000C != 0 {
                // Compressed or encrypted: try zlib for compression only.
                if fflags & 0x0004 != 0 {
                    continue;
                }
            }
            if fflags & 0x0002 != 0 || flags & 0x80 != 0 {
                f = unsync(&f);
            }
            if fflags & 0x0001 != 0 {
                f = f.get(4..).unwrap_or(&[]).to_vec();
            }
            if fflags & 0x0008 != 0 {
                let mut out = Vec::new();
                if photo_deflate::inflate_zlib(&f, &mut out, crate::MAX_VALUE, false).is_err() {
                    continue;
                }
                f = out;
            }
        } else if major == 3 && fflags & 0x00C0 != 0 {
            continue;
        }
        frame(&id, &f, &group, m);
    }
}

fn frame(id: &str, f: &[u8], group: &str, m: &mut Metadata) {
    let Some((&enc, rest)) = f.split_first() else {
        return;
    };
    if id == "TXXX" || id == "TXX" {
        let (desc, value) = split_term(enc, rest);
        let desc = decode_text(enc, desc);
        let value = clean(
            &decode_text(enc, value)
                .trim_end_matches('\0')
                .replace('\0', "/"),
        );
        m.text(group, "UserDefinedText", format!("({desc}) {value}"));
        // Aliases matching ffprobe's tag mapping (encoder, software, ...).
        let alias = match desc.to_ascii_lowercase().as_str() {
            "encoder" | "encoded_by" | "encodedby" => "Encoder",
            "software" | "writing_library" => "Software",
            "creator_tool" | "creatortool" => "CreatorTool",
            "comment" => "Comment",
            "description" => "Description",
            _ => "",
        };
        if !alias.is_empty() {
            m.text(group, alias, value);
        }
        return;
    }
    if id == "COMM" || id == "COM" {
        if rest.len() < 3 {
            return;
        }
        let (_desc, text) = split_term(enc, &rest[3..]);
        m.text(group, "Comment", clean(&decode_text(enc, text)));
        return;
    }
    if id == "PRIV" {
        let (owner, data) = split_term(0, f);
        if owner == b"XMP" {
            crate::xmp::read(data, m);
        }
        return;
    }
    if id.starts_with('T') {
        let value = clean(
            &decode_text(enc, rest)
                .trim_end_matches('\0')
                .replace('\0', "/"),
        );
        if let Some(name) = frame_name(id) {
            m.text(group, name, value.clone());
        } else {
            m.text(group, &tag_name(id), value.clone());
        }
        // TSSE is the "encoder" tag in ffprobe; keep that alias.
        if id == "TSSE" || id == "TSS" {
            m.text(group, "Encoder", value);
        }
    }
}

/// ID3v1 at the end of the file (128 bytes starting "TAG").
pub(crate) fn v1(d: &[u8], m: &mut Metadata) -> usize {
    if d.len() < 128 || &d[d.len() - 128..d.len() - 125] != b"TAG" {
        return 0;
    }
    let t = &d[d.len() - 128..];
    let field =
        |a: usize, b: usize| clean(&latin1(t[a..b].split(|&c| c == 0).next().unwrap_or(&[])));
    m.text("ID3v1", "Title", field(3, 33));
    m.text("ID3v1", "Artist", field(33, 63));
    m.text("ID3v1", "Album", field(63, 93));
    m.text("ID3v1", "Year", field(93, 97));
    let comment_end = if t[125] == 0 && t[126] != 0 { 125 } else { 127 };
    m.text("ID3v1", "Comment", field(97, comment_end));
    128
}
