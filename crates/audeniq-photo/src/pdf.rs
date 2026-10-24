//! PDF sanitization: rasterize every page and rebuild an image-only PDF.
//!
//! [`sanitize_pdf`] does it natively (photo-pdf, merged from hayro, replaces
//! `pdfinfo` + `pdftoppm`). The Poppler-based helpers ([`pages_from_pdfinfo`],
//! [`pdftoppm_args`], [`image_only_pdf`]) remain for callers that still run
//! Poppler.

use crate::sanitize::{MAX_OUTPUT, pixels};
use crate::{Error, Result, guard};
use photo_core::{Deadline, Image, PixelFormat};
use photo_pdf::photo_pdf_interpret::InterpreterSettings;
use photo_pdf::photo_pdf_syntax::{LoadPdfError, Pdf};
use std::fmt::Write as _;

/// Long side of each rendered page (`pdftoppm -scale-to 2048`).
pub const RENDER_SIDE: u16 = 2048;

/// What `pdfinfo` used to report that the sanitizer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfInfo {
    pub pages: u32,
    pub encrypted: bool,
}

/// Parse a PDF and report its page count and encryption.
pub fn info(data: &[u8]) -> Result<PdfInfo> {
    guard(|| {
        let pdf = open(data)?;
        Ok(PdfInfo {
            pages: u32::try_from(pdf.pages().len()).unwrap_or(u32::MAX),
            encrypted: pdf.xref().is_encrypted(),
        })
    })
}

fn open(data: &[u8]) -> Result<Pdf> {
    match Pdf::new(data.to_vec()) {
        Ok(pdf) => Ok(pdf),
        Err(LoadPdfError::Decryption(_)) => Err(Error::Invalid("encrypted or oversized document")),
        Err(LoadPdfError::Invalid) => Err(Error::Invalid("not a readable PDF")),
    }
}

/// Target size for a page box of `w`×`h` points: long side
/// [`RENDER_SIDE`], aspect kept (short side rounded up like pdftoppm).
pub fn render_size(w: f32, h: f32) -> Option<(u16, u16)> {
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let side = f64::from(RENDER_SIDE);
    let short =
        |a: f32, b: f32| ((f64::from(a) * side / f64::from(b)).ceil()).clamp(1.0, side) as u16;
    Some(if w >= h {
        (RENDER_SIDE, short(h, w))
    } else {
        (short(w, h), RENDER_SIDE)
    })
}

/// Render one page (0-based) the way [`sanitize_pdf`] does.
pub fn render_page(data: &[u8], index: usize) -> Result<Image> {
    guard(|| {
        let pdf = open(data)?;
        let page = pdf
            .pages()
            .get(index)
            .ok_or(Error::Invalid("page out of range"))?;
        let (w, h) = page.render_dimensions();
        let (w, h) = render_size(w, h).ok_or(Error::Invalid("page size"))?;
        let cache = photo_pdf::RenderCache::new();
        let data = photo_pdf::render_rgb(page, &cache, &InterpreterSettings::default(), w, h)
            .ok_or(Error::Invalid("page could not be rendered"))?;
        Ok(Image {
            width: u32::from(w),
            height: u32::from(h),
            format: PixelFormat::Rgb8,
            data,
        })
    })
}

/// Open, check (not encrypted, `1..=32` pages, pixel budget) and hand the
/// page sizes plus a page renderer to `f`.
fn with_pages<R>(
    data: &[u8],
    f: impl FnOnce(&[(u16, u16)], &mut dyn FnMut(usize) -> Result<Image>) -> Result<R>,
) -> Result<R> {
    let pdf = open(data)?;
    let pages = pdf.pages();
    if !(1..=MAX_PAGES as usize).contains(&pages.len()) || pdf.xref().is_encrypted() {
        return Err(Error::Invalid("encrypted or oversized document"));
    }
    let sizes = pages
        .iter()
        .map(|p| {
            let (w, h) = p.render_dimensions();
            render_size(w, h).ok_or(Error::Invalid("page size"))
        })
        .collect::<Result<Vec<_>>>()?;
    // Refuse over-budget documents before rendering anything.
    let total: u64 = sizes
        .iter()
        .map(|&(w, h)| u64::from(w) * u64::from(h))
        .sum();
    if total > MAX_TOTAL_PIXELS {
        return Err(Error::Limit("document pixel budget exceeded"));
    }
    let settings = InterpreterSettings::default();
    let cache = photo_pdf::RenderCache::new();
    let mut render = |i: usize| {
        let (w, h) = sizes[i];
        let data = photo_pdf::render_rgb(&pages[i], &cache, &settings, w, h)
            .ok_or(Error::Invalid("page could not be rendered"))?;
        Ok(Image {
            width: u32::from(w),
            height: u32::from(h),
            format: PixelFormat::Rgb8,
            data,
        })
    };
    f(&sizes, &mut render)
}

/// Rasterize every page of an untrusted PDF and rebuild it as an
/// image-only PDF (the former `pdfinfo` → `pdftoppm` → [`image_only_pdf`]
/// pipeline in one pass, without the intermediate JPEG generation).
///
/// Refuses encrypted documents, page counts outside `1..=32` and documents
/// over the 64 MP budget, like the Poppler pipeline. Rendering is not
/// interruptible inside a page; services handling hostile input should
/// split the work: [`rasterize_frames`] in a time- and memory-limited child
/// process, [`image_only_pdf_from_frames`] in the parent.
pub fn sanitize_pdf(data: &[u8], deadline: &Deadline) -> Result<Vec<u8>> {
    guard(|| {
        with_pages(data, |sizes, render| {
            write_image_pdf(sizes.len(), true, deadline, render)
        })
    })
}

/// Magic of the raster frame stream between [`rasterize_frames`] and
/// [`image_only_pdf_from_frames`].
pub const FRAMES_MAGIC: &[u8; 8] = b"APDFRGB1";

/// Untrusted half of [`sanitize_pdf`]: parse and rasterize, writing raw
/// pages to `out` as `FRAMES_MAGIC`, `u32` page count, then per page `u32`
/// width, `u32` height (little endian) and `width * height` RGB8 pixels.
/// Only pixels leave this step, so whatever the parser does, the parent
/// writes the final document itself. Returns the page count.
pub fn rasterize_frames(
    data: &[u8],
    deadline: &Deadline,
    out: &mut dyn std::io::Write,
) -> Result<u32> {
    guard(|| {
        with_pages(data, |sizes, render| {
            let io = |_| Error::Limit("raster output failed");
            out.write_all(FRAMES_MAGIC).map_err(io)?;
            out.write_all(&(sizes.len() as u32).to_le_bytes())
                .map_err(io)?;
            for i in 0..sizes.len() {
                deadline.check()?;
                let image = render(i)?;
                out.write_all(&image.width.to_le_bytes()).map_err(io)?;
                out.write_all(&image.height.to_le_bytes()).map_err(io)?;
                out.write_all(&image.data).map_err(io)?;
            }
            out.flush().map_err(io)?;
            Ok(sizes.len() as u32)
        })
    })
}

/// Trusted half of [`sanitize_pdf`]: validate a frame stream from
/// [`rasterize_frames`] (page count, page sizes, pixel budget, exact length)
/// and write the image-only PDF. Reads one page at a time.
pub fn image_only_pdf_from_frames(
    input: &mut dyn std::io::Read,
    deadline: &Deadline,
) -> Result<Vec<u8>> {
    guard(|| {
        let bad = |_| Error::Invalid("malformed raster stream");
        let mut header = [0u8; 12];
        input.read_exact(&mut header).map_err(bad)?;
        if &header[..8] != FRAMES_MAGIC {
            return Err(Error::Invalid("malformed raster stream"));
        }
        let count = u32::from_le_bytes(header[8..12].try_into().expect("4 bytes"));
        if !(1..=MAX_PAGES).contains(&count) {
            return Err(Error::Invalid("encrypted or oversized document"));
        }
        let side = u32::from(RENDER_SIDE);
        // Reading a page is cheap: no helper thread, one raster at a time.
        let pdf = write_image_pdf(count as usize, false, deadline, |_| {
            let mut dims = [0u8; 8];
            input.read_exact(&mut dims).map_err(bad)?;
            let w = u32::from_le_bytes(dims[..4].try_into().expect("4 bytes"));
            let h = u32::from_le_bytes(dims[4..].try_into().expect("4 bytes"));
            if !(1..=side).contains(&w) || !(1..=side).contains(&h) {
                return Err(Error::Invalid("malformed raster stream"));
            }
            let mut data = vec![0u8; w as usize * h as usize * 3];
            input.read_exact(&mut data).map_err(bad)?;
            Ok(Image {
                width: w,
                height: h,
                format: PixelFormat::Rgb8,
                data,
            })
        })?;
        // Nothing may follow the last page.
        let mut probe = [0u8; 1];
        match input.read(&mut probe) {
            Ok(0) => Ok(pdf),
            _ => Err(Error::Invalid("malformed raster stream")),
        }
    })
}

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
            .rfind(|(k, _)| *k == name)
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
    [
        "-q",
        "-jpeg",
        "-r",
        "110",
        "-scale-to",
        "2048",
        "-f",
        "1",
        "-l",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(pages.to_string()))
    .collect()
}

/// Write a PDF that contains only pages, JPEG image XObjects and the
/// drawing streams placing them. Each raster goes through [`pixels`].
pub fn image_only_pdf(rasters: &[Vec<u8>], deadline: &Deadline) -> Result<Vec<u8>> {
    guard(|| {
        write_image_pdf(rasters.len(), true, deadline, |i| {
            pixels(&rasters[i], deadline)
        })
    })
}

/// Write `count` pages, each one JPEG image XObject filling the page.
///
/// With `overlap`, pages are produced on the calling thread (the renderer's
/// caches are not thread-safe) while a helper thread JPEG-encodes and writes
/// the previous page, so at most two rasters are alive. Without it, or
/// without a helper thread (the OS refused one), the same work runs in turn
/// with one raster alive; the output is identical either way.
fn write_image_pdf(
    count: usize,
    overlap: bool,
    deadline: &Deadline,
    mut page_image: impl FnMut(usize) -> Result<Image>,
) -> Result<Vec<u8>> {
    // Rendered pages pass the pixel budget here, before any encoding.
    let mut total: u64 = 0;
    let mut next = |i: usize| -> Result<Image> {
        deadline.check()?;
        let image = page_image(i)?;
        total += u64::from(image.width) * u64::from(image.height);
        if total > MAX_TOTAL_PIXELS {
            return Err(Error::Limit("document pixel budget exceeded"));
        }
        Ok(image)
    };
    if !overlap || count < 2 {
        return write_pages(count, (0..count).map(next));
    }
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Image>(0);
        let writer = std::thread::Builder::new()
            .spawn_scoped(scope, move || write_pages(count, rx.into_iter().map(Ok)));
        let writer = match writer {
            Ok(w) => w,
            Err(_) => return write_pages(count, (0..count).map(&mut next)),
        };
        let mut failed = None;
        for i in 0..count {
            match next(i) {
                // A closed channel means the writer stopped on an error,
                // which joining it returns.
                Ok(image) => {
                    if tx.send(image).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        drop(tx);
        let written = writer.join().map_err(|_| Error::Internal)?;
        match failed {
            Some(e) => Err(e),
            None => written,
        }
    })
}

/// Encode and write `count` pages taken from `pages`, in order.
fn write_pages(count: usize, pages: impl Iterator<Item = Result<Image>>) -> Result<Vec<u8>> {
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
    let kids: Vec<String> = (0..count).map(|i| format!("{} 0 R", 3 + i * 3)).collect();
    obj(
        &mut out,
        2,
        format!(
            "<< /Type /Pages /Count {} /Kids [{}] >>",
            count,
            kids.join(" ")
        )
        .as_bytes(),
    )?;
    let mut written = 0;
    for (i, image) in pages.enumerate().take(count) {
        let image = image?;
        let (w, h) = (image.width, image.height);
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
        written += 1;
    }
    if written != count {
        return Err(Error::Invalid("page could not be rendered"));
    }
    let start = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for off in &offsets[1..] {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    if out.is_empty() || out.len() > MAX_OUTPUT {
        return Err(Error::Limit("output exceeded limit"));
    }
    Ok(out)
}
