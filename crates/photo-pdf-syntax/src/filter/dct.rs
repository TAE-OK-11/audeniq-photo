use crate::object::Dict;
use crate::object::dict::keys::COLOR_TRANSFORM;
use crate::object::stream::{FilterResult, ImageColorSpace, ImageData, ImageDecodeParams};
use alloc::borrow::Cow;
use photo_core::{Deadline, Limits, PixelFormat};

/// Largest embedded JPEG decoded (pixels); a 600 dpi A3 scan fits.
const MAX_PIXELS: u64 = 128_000_000;

pub(crate) fn decode(
    data: &[u8],
    params: &Dict<'_>,
    image_params: &ImageDecodeParams,
) -> Option<FilterResult<'static>> {
    if image_params.width > u16::MAX as u32 || image_params.height > u16::MAX as u32 {
        return None;
    }
    // Only 1, 3 and 4 component JPEGs exist in practice; refuse the rest.
    if image_params
        .num_components
        .is_some_and(|n| !matches!(n, 1 | 3 | 4))
    {
        return None;
    }

    // Some PDFs have weird JPEGs where the JPEG metadata is completely wrong
    // (for example indicating that one of the dimensions is u16::MAX), but the
    // metadata in the PDF image dictionary is correct. Therefore, we first
    // validate the JPEG metadata and patch the data if any of the dimensions
    // are too large (if they are too small, they will just be padded later on).
    let data = maybe_patch_jpeg_dimensions(data, image_params)?;

    let limits = Limits {
        max_width: u16::MAX as u32,
        max_height: u16::MAX as u32,
        max_pixels: MAX_PIXELS,
        max_alloc: (MAX_PIXELS as usize) * 4,
    };
    let opts = photo_jpeg::DecodeOptions {
        luma_only: false,
        // `/ColorTransform 0` leaves YCbCr samples untransformed.
        keep_ycbcr: params.get::<u8>(COLOR_TRANSFORM) == Some(0),
        // PDF applies its own `/Decode` array to CMYK samples.
        invert_cmyk: false,
    };
    let (_, image) = photo_jpeg::decode_with(&data, &limits, &Deadline::NONE, &opts).ok()?;

    let image_data = ImageData {
        alpha: None,
        color_space: Some(match image.format {
            PixelFormat::Gray8 => ImageColorSpace::Gray,
            PixelFormat::Cmyk8 => ImageColorSpace::Cmyk,
            _ => ImageColorSpace::Rgb,
        }),
        bits_per_component: 8,
        width: image.width,
        height: image.height,
    };

    Some(FilterResult {
        data: Cow::Owned(image.data),
        image_data: Some(image_data),
    })
}

fn maybe_patch_jpeg_dimensions<'a>(
    data: &'a [u8],
    image_params: &ImageDecodeParams,
) -> Option<Cow<'a, [u8]>> {
    let sof_offset = find_sof_marker(data)?;

    let height_offset = sof_offset.checked_add(5)?;
    let width_offset = sof_offset.checked_add(7)?;

    let jpeg_height = u16::from_be_bytes([
        *data.get(height_offset)?,
        *data.get(height_offset.checked_add(1)?)?,
    ]);
    let jpeg_width = u16::from_be_bytes([
        *data.get(width_offset)?,
        *data.get(width_offset.checked_add(1)?)?,
    ]);

    let jpeg_area = (jpeg_width as usize).checked_mul(jpeg_height as usize)?;
    let image_area = (image_params.width as usize).checked_mul(image_params.height as usize)?;
    let need_patch = jpeg_area > image_area;

    if !need_patch {
        return Some(Cow::Borrowed(data));
    }

    let target_w = (image_params.width as u16).to_be_bytes();
    let target_h = (image_params.height as u16).to_be_bytes();

    let mut patched = data.to_vec();
    patched[height_offset..height_offset.checked_add(2)?].copy_from_slice(&target_h);
    patched[width_offset..width_offset.checked_add(2)?].copy_from_slice(&target_w);

    Some(Cow::Owned(patched))
}

fn find_sof_marker(data: &[u8]) -> Option<usize> {
    let mut i = 0_usize;

    while i.checked_add(1).is_some_and(|next| next < data.len()) {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }

        let marker = data[i + 1];

        // Note: Not sure if 100% correct/robust, is AI-generated.
        match marker {
            // All SOF markers carry dimensions: SOF0–SOF15, excluding
            // 0xC4 (DHT), 0xC8 (JPG), 0xCC (DAC) which are not frame markers.
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                return Some(i);
            }
            // Skip padding bytes (0xFF followed by 0xFF).
            0xFF => {
                i += 1;

                continue;
            }
            // SOI (0xD8), EOI (0xD9), TEM (0x01) and stuffed byte (0x00)
            // are standalone markers with no payload.
            0xD8 | 0xD9 | 0x01 | 0x00 => {
                i += 2;

                continue;
            }
            // All other markers have a 2-byte length field — skip over them.
            _ => {
                let len_start = i.checked_add(2)?;
                let len_end = i.checked_add(3)?;
                let seg_len =
                    u16::from_be_bytes([*data.get(len_start)?, *data.get(len_end)?]) as usize;

                i = i.checked_add(2)?.checked_add(seg_len)?;
            }
        }
    }

    None
}
