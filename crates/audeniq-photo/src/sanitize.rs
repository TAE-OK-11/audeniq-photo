//! Port of the backend's `sanitize-upload.py`: decode the frozen bytes,
//! apply EXIF orientation, convert any embedded ICC profile to sRGB pixels
//! and write a brand-new file that carries nothing but pixels.

use crate::{Error, Format, Result, guard, orient};
use photo_core::{Deadline, Image, Limits, PixelFormat};
use photo_deflate::Level;

/// Sanitizer limits (Pillow `MAX_IMAGE_PIXELS` and the script's 8000 px).
pub const MAX_PIXELS: u64 = 40_000_000;
pub const MAX_SIDE: u32 = 8000;
pub const MAX_OUTPUT: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    /// `application/x-audeniq-signature`: strict PNG, re-encoded as PNG.
    Signature,
}

impl Kind {
    pub fn from_mime(mime: &str) -> Option<Kind> {
        match mime {
            "image/png" => Some(Kind::Png),
            "image/jpeg" => Some(Kind::Jpeg),
            "application/x-audeniq-signature" => Some(Kind::Signature),
            _ => None,
        }
    }
}

/// A decoded image with the metadata the sanitizer needs.
pub struct Decoded {
    pub format: Format,
    pub image: Image,
    pub icc_profile: Option<Vec<u8>>,
    pub orientation: Option<i64>,
}

fn exif_orientation(exif: &[u8]) -> Option<i64> {
    // Only IFD0 tag 0x0112 matters here.
    let le = match exif.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b = exif.get(o..o + 2)?;
        Some(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b = exif.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for i in 0..n.min(1000) {
        let e = ifd + 2 + i * 12;
        if u16_at(e)? == 0x0112 {
            return match u16_at(e + 2)? {
                3 => u16_at(e + 8).map(i64::from),
                4 => u32_at(e + 8).map(i64::from),
                _ => None,
            };
        }
    }
    None
}

/// Pillow reads tiff:Orientation from XMP when EXIF has none.
fn xmp_orientation(xmp: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(xmp).ok()?;
    let i = s.find("tiff:Orientation")?;
    let rest = &s[i + 16..];
    let rest = rest.trim_start_matches(|c: char| {
        c == '=' || c == '>' || c == '"' || c == '\'' || c.is_whitespace()
    });
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Decode PNG or JPEG with sanitizer limits and gather orientation/ICC.
pub fn decode_image(data: &[u8], limits: &Limits, deadline: &Deadline) -> Result<Decoded> {
    let format = Format::detect(data).ok_or(Error::Invalid("static JPEG/PNG required"))?;
    match format {
        Format::Png => {
            let (info, image) = photo_png::decode(data, limits, deadline)?;
            if info.animation_frames.is_some_and(|n| n != 1) {
                return Err(Error::Invalid("static JPEG/PNG required"));
            }
            let mut orientation = info.exif.as_deref().and_then(exif_orientation);
            for t in &info.texts {
                if orientation.is_some() {
                    break;
                }
                if t.keyword == "Raw profile type exif" || t.keyword == "Raw profile type APP1" {
                    orientation = raw_profile(&t.text).and_then(|b| {
                        let body = b.strip_prefix(b"Exif\0\0").map(<[u8]>::to_vec).unwrap_or(b);
                        exif_orientation(&body)
                    });
                }
            }
            if orientation.is_none() {
                orientation = info
                    .texts
                    .iter()
                    .find(|t| t.keyword == "XML:com.adobe.xmp")
                    .and_then(|t| xmp_orientation(t.text.as_bytes()));
            }
            Ok(Decoded {
                format,
                image,
                icc_profile: info.icc_profile.filter(|p| !p.is_empty()),
                orientation,
            })
        }
        Format::Jpeg => {
            let (segs, _) = photo_jpeg::segments(data)?;
            // Pillow opens multi-picture JPEGs as MPO, which the sanitizer refused.
            let mpo = segs.iter().any(|s| {
                s.marker == 0xE2
                    && s.data.starts_with(b"MPF\0")
                    && mpf_images(&s.data[4..]).is_some_and(|n| n > 1)
            });
            if mpo {
                return Err(Error::Invalid("static JPEG/PNG required"));
            }
            let (info, image) = photo_jpeg::decode(data, limits, deadline)?;
            let orientation = info
                .exif
                .as_deref()
                .and_then(exif_orientation)
                .or_else(|| info.xmp.as_deref().and_then(xmp_orientation));
            Ok(Decoded {
                format,
                image,
                icc_profile: info.icc_profile.filter(|p| !p.is_empty()),
                orientation,
            })
        }
    }
}

fn mpf_images(tiff: &[u8]) -> Option<u32> {
    let le = match tiff.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let rd16 = |o: usize| {
        tiff.get(o..o + 2).map(|b| {
            if le {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            }
        })
    };
    let rd32 = |o: usize| {
        tiff.get(o..o + 4).map(|b| {
            let a = [b[0], b[1], b[2], b[3]];
            if le {
                u32::from_le_bytes(a)
            } else {
                u32::from_be_bytes(a)
            }
        })
    };
    let ifd = rd32(4)? as usize;
    let n = rd16(ifd)? as usize;
    for i in 0..n.min(100) {
        let e = ifd + 2 + i * 12;
        if rd16(e)? == 0xB001 {
            return rd32(e + 8);
        }
    }
    None
}

fn raw_profile(text: &str) -> Option<Vec<u8>> {
    let mut lines = text.splitn(4, '\n');
    lines.next()?;
    lines.next()?;
    let len: usize = lines.next()?.trim().parse().ok()?;
    if len > 1 << 20 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    let mut hi = None;
    for c in lines.next()?.chars() {
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

/// The script's `pixels()`: static JPEG/PNG → orientation-baked,
/// sRGB, metadata-free RGB8 pixels.
pub fn pixels(data: &[u8], deadline: &Deadline) -> Result<Image> {
    let limits = Limits {
        max_pixels: MAX_PIXELS,
        max_alloc: 256 * 1024 * 1024,
        ..Limits::default()
    };
    let d = decode_image(data, &limits, deadline)?;
    let mut image = orient::apply(d.image, d.orientation.unwrap_or(1));
    if image.width > MAX_SIDE || image.height > MAX_SIDE {
        return Err(Error::Invalid("image dimensions exceeded"));
    }
    deadline.check()?;
    if let Some(icc) = &d.icc_profile {
        let profile =
            photo_icc::Profile::parse(icc).map_err(|_| Error::Invalid("invalid ICC profile"))?;
        // Pillow modes: L/LA → gray, RGB/RGBA/P → RGB, CMYK.
        let (src, channels) = match image.format {
            PixelFormat::Gray8 => (std::mem::take(&mut image.data), 1),
            PixelFormat::GrayAlpha8 => (
                image.data.as_chunks::<2>().0.iter().map(|p| p[0]).collect(),
                1,
            ),
            PixelFormat::Rgb8 => (std::mem::take(&mut image.data), 3),
            PixelFormat::Rgba8 => (
                image
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect(),
                3,
            ),
            PixelFormat::Cmyk8 => (std::mem::take(&mut image.data), 4),
        };
        let t = photo_icc::Transform::to_srgb(&profile, channels)
            .map_err(|_| Error::Invalid("ICC profile does not apply to this image"))?;
        let pixels = image.width as usize * image.height as usize;
        let out = if channels == 3 {
            let mut v = src;
            t.convert_rgb_in_place(&mut v)?;
            v
        } else {
            let mut v = vec![0u8; pixels * 3];
            for (s, d) in src.chunks(channels * 4096).zip(v.chunks_mut(3 * 4096)) {
                t.convert(s, d);
            }
            v
        };
        image = Image {
            width: image.width,
            height: image.height,
            format: PixelFormat::Rgb8,
            data: out,
        };
    } else {
        image = image.into_rgb8();
    }
    Ok(image)
}

/// Sanitize one image upload. Returns the new file's bytes.
pub fn sanitize(data: &[u8], kind: Kind, deadline: &Deadline) -> Result<Vec<u8>> {
    guard(|| {
        if kind == Kind::Signature {
            photo_png::validate_signature(data)?;
        }
        let clean = pixels(data, deadline)?;
        deadline.check()?;
        let out = match kind {
            Kind::Png | Kind::Signature => photo_png::encode(&clean, Level::DEFAULT)?,
            Kind::Jpeg => photo_jpeg::encode(&clean, 95, photo_jpeg::Subsampling::S444)?,
        };
        if out.is_empty() || out.len() > MAX_OUTPUT {
            return Err(Error::Limit("output exceeded limit"));
        }
        Ok(out)
    })
}

/// Encode RGB/gray pixels as a metadata-free PNG (zlib level 6).
pub fn encode_png(img: &Image) -> Result<Vec<u8>> {
    Ok(photo_png::encode(img, Level::DEFAULT)?)
}

/// Encode RGB/gray pixels as a baseline JFIF JPEG (4:4:4 at q >= 90).
pub fn encode_jpeg(img: &Image, quality: u8) -> Result<Vec<u8>> {
    let sub = if quality >= 90 {
        photo_jpeg::Subsampling::S444
    } else {
        photo_jpeg::Subsampling::S420
    };
    Ok(photo_jpeg::encode(img, quality, sub)?)
}
