//! Metadata extraction modelled on ExifTool: the same tag names and
//! family-1 group names (`-G1`), numeric values as with `-n`.
//!
//! Covered containers are the ones the backend accepts: JPEG and PNG
//! images; WAV (RIFF/BWF), AIFF, FLAC, MP4/M4A, WavPack and TTA audio.
//! Embedded directories: EXIF/TIFF, XMP, ICC, PNG text, ID3v1/v2, APEv2,
//! Vorbis comments, QuickTime item lists, RIFF INFO and bext.
//!
//! Damaged metadata blocks are skipped (ExifTool warns and continues); only
//! an unrecognized or unreadable container is an error.
#![forbid(unsafe_code)]

mod ape;
mod audio;
mod exif;
mod file;
mod id3;
mod image;
mod mp4;
mod xmp;

pub use file::read_path;
use photo_core::{Error, Result};

/// Bounds on what one file may contribute.
pub(crate) const MAX_TAGS: usize = 4096;
pub(crate) const MAX_VALUE: usize = 1024 * 1024;
pub(crate) const MAX_TOTAL: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Real(f64),
    Text(String),
    List(Vec<String>),
}

impl Value {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Text(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    fn size(&self) -> usize {
        match self {
            Value::Text(s) => s.len(),
            Value::List(v) => v.iter().map(String::len).sum(),
            _ => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    /// ExifTool family-1 group (e.g. `IFD0`, `XMP-dc`, `PNG`, `Vorbis`).
    pub group: String,
    /// ExifTool tag name (e.g. `Software`, `ColorSpace`).
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Jpeg,
    Png,
    Wav,
    Aiff,
    Flac,
    Mp4,
    WavPack,
    Tta,
}

impl FileType {
    pub fn detect(data: &[u8]) -> Option<FileType> {
        let d = data;
        if d.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(FileType::Jpeg);
        }
        if d.starts_with(&photo_png::SIGNATURE) {
            return Some(FileType::Png);
        }
        if d.len() >= 12 && (&d[..4] == b"RIFF" || &d[..4] == b"RF64") && &d[8..12] == b"WAVE" {
            return Some(FileType::Wav);
        }
        if d.len() >= 12 && &d[..4] == b"FORM" && (&d[8..12] == b"AIFF" || &d[8..12] == b"AIFC") {
            return Some(FileType::Aiff);
        }
        if d.len() >= 12 && &d[4..8] == b"ftyp" {
            return Some(FileType::Mp4);
        }
        if d.starts_with(b"wvpk") {
            return Some(FileType::WavPack);
        }
        // FLAC and TTA may be preceded by an ID3v2 tag.
        let body = &d[id3::skip_header(d)..];
        if body.starts_with(b"fLaC") {
            return Some(FileType::Flac);
        }
        if body.starts_with(b"TTA1") {
            return Some(FileType::Tta);
        }
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub tags: Vec<Tag>,
    /// Embedded ICC profile bytes, if any (JPEG APP2, PNG iCCP, TIFF tag).
    pub icc_profile: Option<Vec<u8>>,
    /// Problems in individual metadata blocks (skipped, like ExifTool warnings).
    pub warnings: Vec<String>,
    total: usize,
}

impl Metadata {
    pub(crate) fn push(&mut self, group: impl Into<String>, name: impl Into<String>, value: Value) {
        if self.tags.len() >= MAX_TAGS {
            return;
        }
        let value = match value {
            Value::Text(mut s) => {
                truncate(&mut s, MAX_VALUE);
                Value::Text(s)
            }
            v => v,
        };
        let size = value.size();
        if self.total + size > MAX_TOTAL {
            return;
        }
        self.total += size;
        self.tags.push(Tag {
            group: group.into(),
            name: name.into(),
            value,
        });
    }

    pub(crate) fn text(&mut self, group: &str, name: &str, s: String) {
        if !s.is_empty() {
            self.push(group, name, Value::Text(s));
        }
    }

    pub(crate) fn warn(&mut self, what: impl Into<String>) {
        if self.warnings.len() < 64 {
            self.warnings.push(what.into());
        }
    }

    /// All tags with this name, in extraction order.
    pub fn get<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Tag> + 'a {
        self.tags.iter().filter(move |t| t.name == name)
    }

    pub(crate) fn icc(&mut self, profile: &[u8]) {
        if self.icc_profile.is_some() {
            return;
        }
        match photo_icc::Profile::parse(profile) {
            Ok(p) => {
                if let Some(d) = p.description() {
                    self.text("ICC_Profile", "ProfileDescription", d);
                }
                // ExifTool -n prints ProfileID as its 16 bytes in decimal.
                let id: Vec<String> = p.bytes()[84..100].iter().map(u8::to_string).collect();
                self.text("ICC-header", "ProfileID", id.join(" "));
                let cs = String::from_utf8_lossy(&p.color_space)
                    .trim_end()
                    .to_string();
                self.text("ICC-header", "ColorSpaceData", cs);
            }
            Err(_) => self.warn("invalid ICC profile"),
        }
        self.icc_profile = Some(profile.to_vec());
    }
}

pub(crate) fn truncate(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

/// ExifTool `MakeTagName`-style: drop illegal characters, capitalize words.
pub(crate) fn tag_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper = true;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            if upper {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            upper = false;
        } else {
            upper = true;
        }
    }
    if out.is_empty() || out.as_bytes()[0].is_ascii_digit() || out.len() < 2 {
        out.insert_str(0, "Tag");
    }
    out
}

pub(crate) fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

/// Text with trailing NULs and whitespace removed.
pub(crate) fn clean(s: &str) -> String {
    s.trim_end_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

/// Decode UTF-8 if valid, else Latin-1 (ExifTool's charset fallback).
pub(crate) fn utf8_or_latin1(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => latin1(b),
    }
}

/// Read all metadata from a file's bytes.
pub fn read(data: &[u8]) -> Result<Metadata> {
    let kind = FileType::detect(data).ok_or(Error::Unsupported("unknown file type"))?;
    let mut m = Metadata::default();
    match kind {
        FileType::Jpeg => image::jpeg(data, &mut m)?,
        FileType::Png => image::png(data, &mut m)?,
        FileType::Wav => audio::riff(data, &mut m)?,
        FileType::Aiff => audio::aiff(data, &mut m)?,
        FileType::Flac => audio::flac(data, &mut m)?,
        FileType::Mp4 => mp4::read(data, &mut m)?,
        FileType::WavPack => audio::wavpack(data, &mut m)?,
        FileType::Tta => audio::tta(data, &mut m)?,
    }
    Ok(m)
}

/// File type name as ExifTool's `FileType` tag.
pub fn file_type_name(kind: FileType) -> &'static str {
    match kind {
        FileType::Jpeg => "JPEG",
        FileType::Png => "PNG",
        FileType::Wav => "WAV",
        FileType::Aiff => "AIFF",
        FileType::Flac => "FLAC",
        FileType::Mp4 => "M4A",
        FileType::WavPack => "WV",
        FileType::Tta => "TTA",
    }
}
