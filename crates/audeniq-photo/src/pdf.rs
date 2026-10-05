//! PDF sanitization helpers (port of `image_only_pdf` and the pdfinfo /
//! pdftoppm handling in `sanitize-upload.py`). Rasterization itself still
//! runs Poppler; the caller executes it inside its sandbox.

use crate::sanitize::{MAX_OUTPUT, pixels};
use crate::{Error, Result, guard};
use photo_core::Deadline;
use std::fmt::Write as _;

pub const MAX_PAGES: u32 = 32;
pub const MAX_INFO_BYTES: usize = 64 * 1024;
pub const MAX_TOTAL_PIXELS: u64 = 64_000_000;

/// Validate `pdfinfo` output: 1..=32 pages and not encrypted.
pub fn pages_from_pdfinfo(stdout: &[u8]) -> Result<u32> {
    if stdout.len() > MAX_INFO_BYTES {
        return Err(Error::Limit("document info exceeded limit"));
    }
    let text = String::from_utf8_lossy(stdout);
    let field = |name: &str| {
        text.lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| *k == name)
            .last()
            .map(|(_, v)| v.trim().to_string())
    };
    let pages: u32 = field("Pages").and_then(|v| v.parse().ok()).unwrap_or(0);
    let encrypted = field("Encrypted").unwrap_or_default();
    if !(1..=MAX_PAGES).contains(&pages) || !encrypted.starts_with("no") {
        return Err(Error::Invalid("encrypted or oversized document"));
    }
    Ok(pages)
}

/// Arguments for `pdftoppm` (as the script ran it).
pub fn pdftoppm_args(pages: u32) -> Vec<String> {
    ["-q", "-jpeg", "-r", "110", "-scale-to", "2048", "-f", "1", "-l"]
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(pages.to_string()))
        .collect()
}

/// Write a PDF that contains only pages, JPEG image XObjects and the
/// drawing streams placing them. Each raster goes through [`pixels`].
pub fn image_only_pdf(rasters: &[Vec<u8>], deadline: &Deadline) -> Result<Vec<u8>> {
    guard(|| {
        let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = vec![0usize];
        let mut obj = |out: &mut Vec<u8>, n: usize, data: &[u8]| -> Result<()> {
            offsets.push(out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendobj\n");
            if out.len() > MAX_OUTPUT {
                return Err(Error::Limit("document derivative exceeded limit"));
            }
            Ok(())
        };
        obj(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>")?;
        let kids: Vec<String> = (0..rasters.len()).map(|i| format!("{} 0 R", 3 + i * 3)).collect();
        obj(&mut out, 2, format!("<< /Type /Pages /Count {} /Kids [{}] >>", rasters.len(), kids.join(" ")).as_bytes())?;
        let mut total: u64 = 0;
        for (i, raster) in rasters.iter().enumerate() {
            deadline.check()?;
            let image = pixels(raster, deadline)?;
            let (w, h) = (image.width, image.height);
            total += u64::from(w) * u64::from(h);
            if total > MAX_TOTAL_PIXELS {
                return Err(Error::Limit("document pixel budget exceeded"));
            }
            let jpeg = photo_jpeg::encode(&image, 85, photo_jpeg::Subsampling::S420)?;
            drop(image);
            let n = 3 + i * 3;
            let scale = 792.0 / f64::from(w.max(h));
            let (pw, ph) = (f64::from(w) * scale, f64::from(h) * scale);
            let mut page = String::new();
            write!(page, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.4} {ph:.4}] /Resources << /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>", n + 1, n + 2).expect("string");
            obj(&mut out, n, page.as_bytes())?;
            let mut xobj = format!("<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n", jpeg.len()).into_bytes();
            xobj.extend_from_slice(&jpeg);
            xobj.extend_from_slice(b"\nendstream");
            obj(&mut out, n + 1, &xobj)?;
            let drawing = format!("q {pw:.4} 0 0 {ph:.4} 0 0 cm /Im0 Do Q");
            let mut content = format!("<< /Length {} >>\nstream\n", drawing.len()).into_bytes();
            content.extend_from_slice(drawing.as_bytes());
            content.extend_from_slice(b"\nendstream");
            obj(&mut out, n + 2, &content)?;
        }
        let start = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
        for off in &offsets[1..] {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n", offsets.len()).as_bytes());
        if out.is_empty() || out.len() > MAX_OUTPUT {
            return Err(Error::Limit("output exceeded limit"));
        }
        Ok(out)
    })
}
