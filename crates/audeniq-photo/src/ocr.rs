//! Artwork text scan: Tesseract's `--psm 11 -l eng+kor` TSV report from
//! the in-process port (`photo-ocr`).

use crate::{Deadline, Error, Limits, PixelFormat, Result, decode_image, guard};
use photo_ocr::{Model, Pix, models};
use std::sync::OnceLock;

/// The eng and kor models, parsed once per process.
fn models() -> Result<&'static [Model; 2]> {
    static MODELS: OnceLock<Option<[Model; 2]>> = OnceLock::new();
    MODELS
        .get_or_init(|| {
            Some([
                Model::load(models::ENG).ok()?,
                Model::load(models::KOR).ok()?,
            ])
        })
        .as_ref()
        .ok_or(Error::Internal)
}

/// OCR a JPEG/PNG like `tesseract <file> stdout -l eng+kor --psm 11 tsv`
/// (Tesseract 5.3.4 with the `tessdata_fast` models) and return the same
/// TSV text. EXIF orientation is ignored, as Tesseract does.
pub fn ocr_tsv(data: &[u8], deadline: &Deadline) -> Result<String> {
    guard(|| {
        let img = decode_image(data, &Limits::default(), deadline)?.image;
        let (w, h) = (img.width as usize, img.height as usize);
        let pix = match img.format {
            PixelFormat::Cmyk8 => {
                let rgb: Vec<u8> = img
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| photo_core::cmyk_to_rgb([p[0], p[1], p[2], p[3]]))
                    .collect();
                Pix::from_rgb(w, h, &rgb, 3)
            }
            _ => Pix::from_image(&img).ok_or(Error::Invalid("unsupported pixel format"))?,
        };
        let [eng, kor] = models()?;
        photo_ocr::ocr_tsv_until(&pix, &[eng, kor], &|| deadline.check().is_err())
            .ok_or(Error::Limit("deadline"))
    })
}
