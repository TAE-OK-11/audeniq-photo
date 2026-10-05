//! ExifTool-shaped JSON from photo-meta tags.

use crate::{Error, Result, guard};
use photo_meta::{Metadata, Value};
use serde_json::{Map, Value as J};

/// Read all metadata (replacement for running `exiftool` on the file).
pub fn metadata(data: &[u8]) -> Result<Metadata> {
    guard(|| photo_meta::read(data).map_err(Error::from))
}

/// Read metadata from a file without loading audio sample data; suitable
/// for multi-hundred-megabyte masters.
pub fn metadata_file(path: &std::path::Path) -> Result<Metadata> {
    guard(|| photo_meta::read_path(path).map_err(Error::from))
}

/// ExifTool's JSON number rule (`EscapeJSON`): integers up to 15 digits
/// without leading zeros, optional fraction and exponent.
fn looks_numeric(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i += 1;
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let digits = i - start;
    if digits == 0 || digits > 15 || (digits > 1 && b[start] == b'0') {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == f || i - f > 16 {
            return false;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let e = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == e || i - e > 3 {
            return false;
        }
    }
    i == b.len()
}

fn json_scalar(s: &str) -> J {
    if looks_numeric(s) {
        if let Ok(i) = s.parse::<i64>() {
            return J::from(i);
        }
        if let Some(n) = s.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
            return J::Number(n);
        }
    }
    J::String(s.to_string())
}

pub(crate) fn json_value(v: &Value) -> J {
    match v {
        Value::Int(i) => J::from(*i),
        Value::Real(r) => serde_json::Number::from_f64(*r).map_or(J::Null, J::Number),
        Value::Text(s) => json_scalar(s),
        Value::List(l) => J::Array(l.iter().map(|s| json_scalar(s)).collect()),
    }
}

/// The fields `artwork_policy` asked ExifTool for (`-j -n -s`, no groups).
pub const COLOR_FIELDS: &[&str] = &[
    "ColorSpace",
    "ColorType",
    "PhotometricInterpretation",
    "SamplesPerPixel",
    "BitsPerSample",
    "BitDepth",
    "ColorComponents",
    "ProfileDescription",
    "ProfileID",
    "Orientation",
];

/// The fields `provenance` asked ExifTool for (`-j -n -G1 -s`).
pub const PROVENANCE_FIELDS: &[&str] = &[
    "Software",
    "CreatorTool",
    "DigitalSourceType",
    "Description",
    "Comment",
    "Encoder",
    "UserComment",
    "Parameters",
    "GenerationParameters",
    "Prompt",
    "Workflow",
];

fn group_rank(group: &str) -> u8 {
    // The real container first, then EXIF, ICC, XMP; thumbnails last.
    match group {
        "File" | "PNG" => 0,
        "IFD0" | "ExifIFD" => 1,
        "ICC_Profile" | "ICC-header" => 2,
        g if g.starts_with("XMP-") => 3,
        "IFD1" => 9,
        _ => 5,
    }
}

/// Color properties object, one value per field (ExifTool duplicate
/// suppression: the highest-priority group wins).
pub fn color_report(meta: &Metadata) -> Map<String, J> {
    let mut out = Map::new();
    for &field in COLOR_FIELDS {
        if let Some(t) = meta.get(field).min_by_key(|t| group_rank(&t.group)) {
            out.insert(field.to_string(), json_value(&t.value));
        }
    }
    out
}

/// Provenance fields keyed `Group:Name` like `exiftool -G1`. Repeated
/// group/name pairs (e.g. several AIFF comments) get `Group-2:Name` keys so
/// the field name after the last colon stays exact.
pub fn provenance_fields(meta: &Metadata) -> Map<String, J> {
    let mut out = Map::new();
    for t in &meta.tags {
        let base = t.name.split('-').next().unwrap_or(&t.name);
        if !PROVENANCE_FIELDS.contains(&base) {
            continue;
        }
        let mut key = format!("{}:{}", t.group, t.name);
        let mut n = 2;
        while out.contains_key(&key) {
            key = format!("{}-{n}:{}", t.group, t.name);
            n += 1;
        }
        out.insert(key, json_value(&t.value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exiftool_number_rule() {
        for s in ["0", "1", "-5", "65535", "2.2", "1e5", "123456789012345"] {
            assert!(looks_numeric(s), "{s}");
        }
        for s in [
            "",
            "01",
            "1.",
            "8 8 8",
            "0000",
            "1234567890123456",
            "abc",
            "1e1234",
            "0x10",
        ] {
            assert!(!looks_numeric(s), "{s}");
        }
    }
}
