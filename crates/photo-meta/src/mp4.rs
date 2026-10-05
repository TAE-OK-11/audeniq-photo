//! QuickTime/MP4 atoms: moov/udta/meta/ilst item lists, UserData, XMP.

use crate::{Metadata, clean, latin1, tag_name, xmp};
use photo_core::{Error, Result};

const MAX_ATOMS: usize = 100_000;
const MAX_DEPTH: usize = 12;
const XMP_UUID: [u8; 16] = [
    0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF, 0xAC,
];

fn item_name(id: &[u8]) -> Option<String> {
    Some(
        match id {
            b"\xa9too" => "Encoder",
            b"\xa9cmt" => "Comment",
            b"desc" | b"\xa9des" => "Description",
            b"\xa9swr" => "SoftwareVersion",
            b"\xa9enc" => "EncodedBy",
            b"\xa9nam" => "Title",
            b"\xa9ART" => "Artist",
            b"aART" => "AlbumArtist",
            b"\xa9alb" => "Album",
            b"\xa9wrt" => "Composer",
            b"\xa9day" => "ContentCreateDate",
            b"\xa9gen" => "Genre",
            b"cprt" => "Copyright",
            b"\xa9lyr" => "Lyrics",
            _ => return None,
        }
        .to_string(),
    )
}

struct Walker<'m> {
    m: &'m mut Metadata,
    atoms: usize,
}

impl Walker<'_> {
    fn atoms(&mut self, d: &[u8], depth: usize, parent: &[u8]) {
        if depth > MAX_DEPTH {
            return;
        }
        let mut pos = 0;
        while pos + 8 <= d.len() {
            self.atoms += 1;
            if self.atoms > MAX_ATOMS {
                return;
            }
            let mut size = u32::from_be_bytes(d[pos..pos + 4].try_into().expect("4")) as u64;
            let kind = &d[pos + 4..pos + 8];
            let mut hdr = 8;
            if size == 1 {
                let Some(b) = d.get(pos + 8..pos + 16) else {
                    return;
                };
                size = u64::from_be_bytes(b.try_into().expect("8"));
                hdr = 16;
            } else if size == 0 {
                size = (d.len() - pos) as u64;
            }
            if size < hdr as u64 || pos as u64 + size > d.len() as u64 {
                // mdat and other large atoms may be truncated in tests; stop.
                return;
            }
            let body = &d[pos + hdr..pos + size as usize];
            pos += size as usize;
            match kind {
                b"moov" | b"udta" | b"trak" | b"mdia" | b"minf" => {
                    self.atoms(body, depth + 1, kind)
                }
                b"meta" => {
                    // Full box (version/flags) in MP4; QuickTime omits it.
                    let inner = if body.len() >= 8 && &body[4..8] == b"hdlr" {
                        body
                    } else {
                        body.get(4..).unwrap_or(&[])
                    };
                    self.atoms(inner, depth + 1, kind);
                }
                b"ilst" => self.ilst(body),
                b"XMP_" => xmp::read(body, self.m),
                b"uuid" if body.len() >= 16 && body[..16] == XMP_UUID => {
                    xmp::read(&body[16..], self.m)
                }
                _ if parent == b"udta" && kind[0] == 0xA9 => {
                    // QuickTime UserData text: size(2) lang(2) text.
                    if let Some(name) = item_name(kind)
                        && body.len() >= 4
                    {
                        let n = u16::from_be_bytes([body[0], body[1]]) as usize;
                        if let Some(t) = body.get(4..4 + n) {
                            self.m
                                .text("UserData", &name, clean(&String::from_utf8_lossy(t)));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn ilst(&mut self, d: &[u8]) {
        let mut pos = 0;
        while pos + 8 <= d.len() {
            self.atoms += 1;
            if self.atoms > MAX_ATOMS {
                return;
            }
            let size = u32::from_be_bytes(d[pos..pos + 4].try_into().expect("4")) as usize;
            if size < 8 || pos + size > d.len() {
                return;
            }
            let kind = &d[pos + 4..pos + 8];
            let body = &d[pos + 8..pos + size];
            pos += size;
            let (mut name, mut value) = (item_name(kind), None);
            let mut p = 0;
            while p + 8 <= body.len() {
                let s = u32::from_be_bytes(body[p..p + 4].try_into().expect("4")) as usize;
                if s < 8 || p + s > body.len() {
                    break;
                }
                let k = &body[p + 4..p + 8];
                let b = &body[p + 8..p + s];
                match k {
                    b"name" if b.len() >= 4 => name = Some(tag_name(&latin1(&b[4..]))),
                    b"data" if b.len() >= 8 => {
                        let ty = u32::from_be_bytes(b[..4].try_into().expect("4")) & 0x00FF_FFFF;
                        if ty == 1 {
                            value = Some(clean(&String::from_utf8_lossy(&b[8..])));
                        } else if ty == 2 {
                            let u: Vec<u16> = b[8..]
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                .collect();
                            value = Some(clean(&String::from_utf16_lossy(&u)));
                        }
                    }
                    _ => {}
                }
                p += s;
            }
            if let (Some(n), Some(v)) = (name, value) {
                self.m.text("ItemList", &n, v);
            }
        }
    }
}

pub(crate) fn read(d: &[u8], m: &mut Metadata) -> Result<()> {
    if d.len() < 12 || &d[4..8] != b"ftyp" {
        return Err(Error::Invalid("not an MP4 file"));
    }
    let mut w = Walker { m, atoms: 0 };
    w.atoms(d, 0, b"");
    Ok(())
}
