//! EXIF / TIFF IFD reader (ExifTool Exif.pm subset with the same names).

use crate::{Metadata, Value, clean, latin1, utf8_or_latin1};
use std::collections::HashSet;

const MAX_ENTRIES: usize = 1000;
const MAX_IFDS: usize = 32;

/// Tag id → (name, group override for sub-IFD pointers).
fn name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0100 => "ImageWidth",
        0x0101 => "ImageHeight",
        0x0102 => "BitsPerSample",
        0x0103 => "Compression",
        0x0106 => "PhotometricInterpretation",
        0x010E => "ImageDescription",
        0x010F => "Make",
        0x0110 => "Model",
        0x0112 => "Orientation",
        0x0115 => "SamplesPerPixel",
        0x011A => "XResolution",
        0x011B => "YResolution",
        0x0128 => "ResolutionUnit",
        0x0131 => "Software",
        0x0132 => "ModifyDate",
        0x013B => "Artist",
        0x013C => "HostComputer",
        0x0213 => "YCbCrPositioning",
        0x8298 => "Copyright",
        0x829A => "ExposureTime",
        0x829D => "FNumber",
        0x8827 => "ISO",
        0x9000 => "ExifVersion",
        0x9003 => "DateTimeOriginal",
        0x9004 => "CreateDate",
        0x9286 => "UserComment",
        0x9C9B => "XPTitle",
        0x9C9C => "XPComment",
        0x9C9D => "XPAuthor",
        0x9C9E => "XPKeywords",
        0x9C9F => "XPSubject",
        0xA001 => "ColorSpace",
        0xA002 => "ExifImageWidth",
        0xA003 => "ExifImageHeight",
        0xA420 => "ImageUniqueID",
        0xA430 => "OwnerName",
        0xA433 => "LensMake",
        0xA434 => "LensModel",
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn u16(&self, o: usize) -> Option<u16> {
        let b = self.d.get(o..o + 2)?;
        Some(if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    }
    fn u32(&self, o: usize) -> Option<u32> {
        let b = self.d.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }
}

fn type_size(t: u16) -> Option<usize> {
    Some(match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

/// Parse a TIFF structure (EXIF payload). `base_group` is "IFD0" for the
/// main image chain.
pub(crate) fn read(d: &[u8], m: &mut Metadata) {
    if d.len() < 8 {
        m.warn("truncated EXIF");
        return;
    }
    let le = match &d[..4] {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => {
            m.warn("invalid EXIF header");
            return;
        }
    };
    let t = Tiff { d, le };
    let mut seen = HashSet::new();
    let mut next = t.u32(4).map(|v| v as usize);
    let mut index = 0;
    while let Some(off) = next {
        if off == 0 || index >= 2 || seen.len() >= MAX_IFDS {
            break;
        }
        let group = if index == 0 { "IFD0" } else { "IFD1" };
        next = ifd(t, off, group, m, &mut seen);
        index += 1;
    }
}

/// Read one IFD; returns the next-IFD offset.
fn ifd(t: Tiff, off: usize, group: &str, m: &mut Metadata, seen: &mut HashSet<usize>) -> Option<usize> {
    if !seen.insert(off) {
        m.warn("EXIF IFD loop");
        return None;
    }
    let n = t.u16(off)? as usize;
    if n > MAX_ENTRIES {
        m.warn("too many EXIF entries");
        return None;
    }
    for i in 0..n {
        let e = off + 2 + i * 12;
        let (Some(id), Some(ty), Some(count)) = (t.u16(e), t.u16(e + 2), t.u32(e + 4)) else {
            m.warn("truncated EXIF IFD");
            return None;
        };
        let Some(size) = type_size(ty) else { continue };
        let Some(total) = (count as usize).checked_mul(size) else { continue };
        let data_off = if total <= 4 { e + 8 } else { t.u32(e + 8)? as usize };
        let Some(raw) = t.d.get(data_off..data_off.saturating_add(total)) else {
            m.warn("EXIF value out of bounds");
            continue;
        };
        match id {
            0x8769 | 0x8825 | 0xA005 => {
                if let Some(p) = t.u32(e + 8) {
                    let sub = match id {
                        0x8769 => "ExifIFD",
                        0x8825 => "GPS",
                        _ => "InteropIFD",
                    };
                    if seen.len() < MAX_IFDS && sub == "ExifIFD" {
                        ifd(t, p as usize, sub, m, seen);
                    }
                }
                continue;
            }
            0x02BC => {
                crate::xmp::read(raw, m);
                continue;
            }
            0x8773 => {
                m.icc(raw);
                continue;
            }
            _ => {}
        }
        let Some(name) = name(id) else { continue };
        let value = match (id, ty) {
            (0x9286, _) => Value::Text(user_comment(raw, t.le)),
            (0x9C9B..=0x9C9F, _) => {
                let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                Value::Text(clean(&String::from_utf16_lossy(&units)))
            }
            (_, 2) => Value::Text(clean(&utf8_or_latin1(raw.split(|&b| b == 0).next().unwrap_or(&[])))),
            (_, 7) => Value::Text(clean(&latin1(raw))),
            _ => numeric(t, ty, raw, count as usize),
        };
        if matches!(&value, Value::Text(s) if s.is_empty()) {
            continue;
        }
        m.push(group, name, value);
    }
    t.u32(off + 2 + n * 12).map(|v| v as usize)
}

fn numeric(t: Tiff, ty: u16, raw: &[u8], count: usize) -> Value {
    let tt = Tiff { d: raw, le: t.le };
    let one = |i: usize| -> Option<f64> {
        Some(match ty {
            1 => f64::from(raw[i]),
            6 => f64::from(raw[i] as i8),
            3 => f64::from(tt.u16(i * 2)?),
            8 => f64::from(tt.u16(i * 2)? as i16),
            4 | 13 => f64::from(tt.u32(i * 4)?),
            9 => f64::from(tt.u32(i * 4)? as i32),
            5 => {
                let (n, d) = (tt.u32(i * 8)?, tt.u32(i * 8 + 4)?);
                if d == 0 { return Some(f64::INFINITY) } else { f64::from(n) / f64::from(d) }
            }
            10 => {
                let (n, d) = (tt.u32(i * 8)? as i32, tt.u32(i * 8 + 4)? as i32);
                if d == 0 { return Some(f64::INFINITY) } else { f64::from(n) / f64::from(d) }
            }
            11 => f64::from(f32::from_bits(tt.u32(i * 4)?)),
            12 => {
                let hi = u64::from(tt.u32(i * 8)?);
                let lo = u64::from(tt.u32(i * 8 + 4)?);
                f64::from_bits(if t.le { lo << 32 | hi } else { hi << 32 | lo })
            }
            _ => return None,
        })
    };
    let vals: Vec<f64> = (0..count.min(64)).filter_map(one).collect();
    let fmt = |v: f64| if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") };
    match vals.as_slice() {
        [v] if v.fract() == 0.0 && v.abs() < 1e15 => Value::Int(*v as i64),
        [v] => Value::Real(*v),
        _ => Value::Text(vals.iter().map(|&v| fmt(v)).collect::<Vec<_>>().join(" ")),
    }
}

/// ExifTool ConvertExifText: 8-byte character code, then text.
fn user_comment(raw: &[u8], le: bool) -> String {
    if raw.len() < 8 {
        return clean(&latin1(raw));
    }
    let (code, body) = raw.split_at(8);
    let s = match code {
        b"UNICODE\0" => {
            // Byte order: BOM if present, else the EXIF byte order.
            let (body, le) = match body {
                [0xFF, 0xFE, rest @ ..] => (rest, true),
                [0xFE, 0xFF, rest @ ..] => (rest, false),
                _ => (body, le),
            };
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => utf8_or_latin1(body),
    };
    s.trim_end_matches(['\0', ' ']).to_string()
}
