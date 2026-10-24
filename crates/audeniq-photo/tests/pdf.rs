//! PDF path. Native: photo-pdf parses and rasterizes, then the image-only
//! PDF is written ([`pdf::sanitize_pdf`]); checked against Poppler where it
//! is installed. Legacy: Poppler rasterizes and Rust writes the derivative
//! (byte comparison with the original Python writer).
use audeniq_photo::{Deadline, pdf};
use std::path::Path;
use std::process::Command;

fn hostile_pdf() -> Vec<u8> {
    build_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /OpenAction 5 0 R /Names << /EmbeddedFiles 6 0 R >> >>",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 4 0 R >>",
        b"<< /Length 23 >>\nstream\n0 0 100 100 re 0.5 g f\nendstream",
        b"<< /S /JavaScript /JS (app.alert('MALICIOUS_MARKER')) >>",
        b"<< /Names [(evil.js) 7 0 R] >>",
        b"<< /Type /Filespec /F (evil.js) /EF << /F 8 0 R >> >>",
        b"<< /Type /EmbeddedFile /Length 16 >>\nstream\nMALICIOUS_MARKER\nendstream",
    ])
}

fn build_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut data = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (n, obj) in objects.iter().enumerate() {
        offsets.push(data.len());
        data.extend_from_slice(format!("{} 0 obj\n", n + 1).as_bytes());
        data.extend_from_slice(obj);
        data.extend_from_slice(b"\nendobj\n");
    }
    let start = data.len();
    data.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for o in &offsets[1..] {
        data.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    data
}

fn contains(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

#[test]
fn javascript_and_embedded_files_do_not_reach_derivative() {
    let dir = std::env::temp_dir().join(format!("audeniq-photo-pdf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("source.pdf");
    std::fs::write(&src, hostile_pdf()).unwrap();
    let Ok(info) = Command::new("pdfinfo").arg(&src).output() else {
        eprintln!("poppler unavailable; skipping");
        return;
    };
    let pages = pdf::pages_from_pdfinfo(&info.stdout).unwrap();
    assert_eq!(pages, 1);
    let prefix = dir.join("raster");
    assert!(
        Command::new("pdftoppm")
            .args(pdf::pdftoppm_args(pages))
            .arg(&src)
            .arg(&prefix)
            .status()
            .unwrap()
            .success()
    );
    let mut rasters: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("raster-")
        })
        .collect();
    rasters.sort();
    let bytes: Vec<Vec<u8>> = rasters.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let out = pdf::image_only_pdf(&bytes, &Deadline::NONE).unwrap();
    for forbidden in [
        &b"/JavaScript"[..],
        b"/OpenAction",
        b"/EmbeddedFile",
        b"MALICIOUS_MARKER",
    ] {
        assert!(!contains(&out, forbidden));
    }
    assert!(contains(&out, b"/Subtype /Image"));
    let target = dir.join("safe.pdf");
    std::fs::write(&target, &out).unwrap();
    let check = Command::new("pdfinfo").arg(&target).output().unwrap();
    assert!(String::from_utf8_lossy(&check.stdout).contains("Pages:           1"));

    // Byte-identical to the original Python writer on the same rasters.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference/sanitize-upload.py");
    let py = format!(
        "import importlib.util,pathlib\ns=importlib.util.spec_from_file_location('s',r'{}')\nm=importlib.util.module_from_spec(s);s.loader.exec_module(m)\nm.image_only_pdf([pathlib.Path(p) for p in {:?}], pathlib.Path(r'{}'))",
        script.display(),
        rasters
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        dir.join("ref.pdf").display()
    );
    if Command::new("python3")
        .args(["-c", &py])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        assert_eq!(
            std::fs::read(dir.join("ref.pdf")).unwrap(),
            out,
            "PDF differs from the Python writer"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pdfinfo_rules() {
    assert_eq!(
        pdf::pages_from_pdfinfo(b"Pages:           3\nEncrypted:       no\n").unwrap(),
        3
    );
    assert!(pdf::pages_from_pdfinfo(b"Pages:           33\nEncrypted:       no\n").is_err());
    assert!(
        pdf::pages_from_pdfinfo(b"Pages:           1\nEncrypted:       yes (print:yes)\n").is_err()
    );
    assert!(pdf::pages_from_pdfinfo(b"Encrypted:       no\n").is_err());
    assert!(pdf::pages_from_pdfinfo(&vec![b'a'; 70_000]).is_err());
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf")
            .join(name),
    )
    .unwrap()
}

/// `n` blank pages.
fn pages_pdf(n: usize) -> Vec<u8> {
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", i + 3)).collect();
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Count {n} /Kids [{}] >>", kids.join(" ")).into_bytes(),
    ];
    for _ in 0..n {
        objects.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>".to_vec());
    }
    let refs: Vec<&[u8]> = objects.iter().map(Vec::as_slice).collect();
    build_pdf(&refs)
}

fn mean_abs_diff(a: &audeniq_photo::Image, b: &audeniq_photo::Image) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let sum: u64 = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    sum as f64 / a.data.len() as f64
}

#[test]
fn native_sanitize_keeps_only_rasters() {
    let src = hostile_pdf();
    let info = pdf::info(&src).unwrap();
    assert_eq!((info.pages, info.encrypted), (1, false));
    let out = pdf::sanitize_pdf(&src, &Deadline::NONE).unwrap();
    for forbidden in [
        &b"/JavaScript"[..],
        b"/OpenAction",
        b"/EmbeddedFile",
        b"MALICIOUS_MARKER",
    ] {
        assert!(!contains(&out, forbidden));
    }
    assert_eq!(pdf::info(&out).unwrap().pages, 1);
    // The page is 50% gray everywhere; so is its raster.
    let page = pdf::render_page(&src, 0).unwrap();
    assert_eq!((page.width, page.height), (2048, 2048));
    assert!(page.data.iter().all(|&v| v.abs_diff(128) <= 1), "gray page");
    // And the derivative re-renders to (nearly) the same pixels.
    let again = pdf::render_page(&out, 0).unwrap();
    assert!(mean_abs_diff(&page, &again) < 1.5);
}

#[test]
fn native_sanitize_refuses_encrypted_and_page_limits() {
    for name in ["encrypted_aes_128.pdf", "encrypted_rc4_rev2.pdf"] {
        let data = fixture(name);
        assert!(pdf::info(&data).unwrap().encrypted, "{name}");
        assert!(pdf::sanitize_pdf(&data, &Deadline::NONE).is_err(), "{name}");
    }
    let out = pdf::sanitize_pdf(&pages_pdf(30), &Deadline::NONE).unwrap();
    assert_eq!(pdf::info(&out).unwrap().pages, 30);
    // 32 pages of 2048x1024 exceed the 64 MP budget.
    assert_eq!(
        pdf::sanitize_pdf(&pages_pdf(32), &Deadline::NONE),
        Err(audeniq_photo::Error::Limit(
            "document pixel budget exceeded"
        ))
    );
    assert!(pdf::sanitize_pdf(&pages_pdf(33), &Deadline::NONE).is_err());
    assert!(pdf::sanitize_pdf(&pages_pdf(0), &Deadline::NONE).is_err());
    assert!(pdf::sanitize_pdf(b"%PDF-1.4\ngarbage", &Deadline::NONE).is_err());
    assert!(pdf::sanitize_pdf(b"", &Deadline::NONE).is_err());
}

#[test]
fn render_size_follows_scale_to() {
    assert_eq!(pdf::render_size(612.0, 792.0), Some((1583, 2048)));
    assert_eq!(pdf::render_size(842.0, 595.0), Some((2048, 1448)));
    assert_eq!(pdf::render_size(100.0, 100.0), Some((2048, 2048)));
    assert_eq!(pdf::render_size(10000.0, 1.0), Some((2048, 1)));
    assert_eq!(pdf::render_size(0.0, 1.0), None);
    assert_eq!(pdf::render_size(f32::NAN, 1.0), None);
}

#[test]
fn native_render_matches_pdftoppm() {
    let dir = std::env::temp_dir().join(format!("audeniq-photo-pdfcmp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, data, tolerance) in [
        ("gray", hostile_pdf(), 1.5),
        ("text", fixture("text_with_rise.pdf"), 6.0),
    ] {
        let src = dir.join(format!("{name}.pdf"));
        std::fs::write(&src, &data).unwrap();
        let prefix = dir.join(name);
        let Ok(status) = Command::new("pdftoppm")
            .args([
                "-q",
                "-png",
                "-r",
                "110",
                "-scale-to",
                "2048",
                "-singlefile",
            ])
            .arg(&src)
            .arg(&prefix)
            .status()
        else {
            eprintln!("poppler unavailable; skipping");
            return;
        };
        assert!(status.success());
        let png = std::fs::read(prefix.with_extension("png")).unwrap();
        let reference = audeniq_photo::sanitize::pixels(&png, &Deadline::NONE).unwrap();
        let ours = pdf::render_page(&data, 0).unwrap();
        let d = mean_abs_diff(&reference, &ours);
        assert!(d < tolerance, "{name}: mean abs diff {d}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn split_frames_pipeline_matches_in_process_sanitize() {
    let src = hostile_pdf();
    let mut frames = Vec::new();
    assert_eq!(
        pdf::rasterize_frames(&src, &Deadline::NONE, &mut frames),
        Ok(1)
    );
    assert_eq!(frames.len(), 12 + 8 + 2048 * 2048 * 3);
    let out = pdf::image_only_pdf_from_frames(&mut frames.as_slice(), &Deadline::NONE).unwrap();
    assert_eq!(out, pdf::sanitize_pdf(&src, &Deadline::NONE).unwrap());
    let mut sink = Vec::new();
    assert!(
        pdf::rasterize_frames(
            &fixture("encrypted_aes_128.pdf"),
            &Deadline::NONE,
            &mut sink
        )
        .is_err()
    );
}

#[test]
fn malformed_frame_streams_are_refused() {
    let frame = |pages: &[(u32, u32)], count: u32| {
        let mut v = pdf::FRAMES_MAGIC.to_vec();
        v.extend_from_slice(&count.to_le_bytes());
        for &(w, h) in pages {
            v.extend_from_slice(&w.to_le_bytes());
            v.extend_from_slice(&h.to_le_bytes());
            v.extend(std::iter::repeat_n(200u8, (w * h * 3) as usize));
        }
        v
    };
    let ok = frame(&[(64, 32), (16, 16)], 2);
    let out = pdf::image_only_pdf_from_frames(&mut ok.as_slice(), &Deadline::NONE).unwrap();
    assert_eq!(pdf::info(&out).unwrap().pages, 2);
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("magic", {
            let mut v = ok.clone();
            v[0] ^= 1;
            v
        }),
        ("zero pages", frame(&[], 0)),
        ("33 pages", frame(&[(1, 1); 33], 33)),
        ("count above frames", frame(&[(8, 8)], 2)),
        ("zero width", frame(&[(0, 8)], 1)),
        ("too wide", frame(&[(2049, 8)], 1)),
        ("truncated", ok[..ok.len() - 1].to_vec()),
        ("trailing", {
            let mut v = ok.clone();
            v.push(0);
            v
        }),
    ];
    // 16 full 2048x2048 pages exceed the 64 MP budget.
    cases.push(("budget", frame(&[(2048, 2048); 16], 16)));
    for (name, bytes) in cases {
        assert!(
            pdf::image_only_pdf_from_frames(&mut bytes.as_slice(), &Deadline::NONE).is_err(),
            "{name}"
        );
    }
}

#[test]
fn page_failures_stop_the_overlapped_writer() {
    // Pages are produced while the previous one is encoded on a helper
    // thread; a bad page in the middle or at the end must fail the whole
    // document with that page's error (and never hang or emit a prefix).
    let img = audeniq_photo::Image {
        width: 64,
        height: 48,
        format: audeniq_photo::PixelFormat::Rgb8,
        data: (0..64 * 48 * 3).map(|i| (i % 251) as u8).collect(),
    };
    let good = audeniq_photo::sanitize::encode_jpeg(&img, 90).unwrap();
    let bad = b"\xFF\xD8\xFF not a jpeg".to_vec();
    let ok =
        pdf::image_only_pdf(&[good.clone(), good.clone(), good.clone()], &Deadline::NONE).unwrap();
    assert_eq!(pdf::info(&ok).unwrap().pages, 3);
    for pages in [
        vec![bad.clone(), good.clone(), good.clone()],
        vec![good.clone(), bad.clone(), good.clone()],
        vec![good.clone(), good.clone(), bad.clone()],
    ] {
        assert!(matches!(
            pdf::image_only_pdf(&pages, &Deadline::NONE),
            Err(audeniq_photo::Error::Invalid(_))
        ));
    }
    let expired = Deadline::after(std::time::Duration::ZERO);
    assert_eq!(
        pdf::image_only_pdf(&[good.clone(), good], &expired),
        Err(audeniq_photo::Error::Limit("deadline"))
    );
}
