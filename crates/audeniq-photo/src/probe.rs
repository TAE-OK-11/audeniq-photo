//! `ffprobe -show_format -show_streams` replacement for cover images.

use crate::{Error, Format, Result, guard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub format: Format,
    pub width: u32,
    pub height: u32,
}

impl Probe {
    /// ffprobe's demuxer name, kept for evidence continuity.
    pub fn format_name(&self) -> &'static str {
        match self.format {
            Format::Png => "png_pipe",
            Format::Jpeg => "jpeg_pipe",
        }
    }
}

/// Read container headers (no pixel decode), like ffprobe's stream info.
pub fn probe(data: &[u8]) -> Result<Probe> {
    guard(|| {
        let format = Format::detect(data).ok_or(Error::Invalid("not a PNG or JPEG file"))?;
        let (width, height) = match format {
            Format::Png => {
                let info = photo_png::read_info(data)?;
                (info.width, info.height)
            }
            Format::Jpeg => {
                let info = photo_jpeg::read_info(data)?;
                (info.frame.width, info.frame.height)
            }
        };
        if width == 0 || height == 0 {
            return Err(Error::Invalid("zero image dimension"));
        }
        Ok(Probe {
            format,
            width,
            height,
        })
    })
}
