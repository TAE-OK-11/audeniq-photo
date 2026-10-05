//! PDF path: Poppler rasterizes (as before), Rust writes the image-only PDF.
//! Port of test_pdf_javascript_and_embedded_files_do_not_reach_derivative,
//! plus a byte comparison with the original Python writer.
use audeniq_photo::{Deadline, pdf};
use std::path::Path;
use std::process::Command;

fn hostile_pdf() -> Vec<u8> {
    let objects: [&[u8]; 8] = [
        b"<< /Type /Catalog /Pages 2 0 R /OpenAction 5 0 R /Names << /EmbeddedFiles 6 0 R >> >>",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 4 0 R >>",
        b"<< /Length 23 >>\nstream\n0 0 100 100 re 0.5 g f\nendstream",
        b"<< /S /JavaScript /JS (app.alert('MALICIOUS_MARKER')) >>",
        b"<< /Names [(evil.js) 7 0 R] >>",
        b"<< /Type /Filespec /F (evil.js) /EF << /F 8 0 R >> >>",
        b"<< /Type /EmbeddedFile /Length 16 >>\nstream\nMALICIOUS_MARKER\nendstream",
    ];
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
    data.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n", offsets.len()).as_bytes());
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
    assert!(Command::new("pdftoppm").args(pdf::pdftoppm_args(pages)).arg(&src).arg(&prefix).status().unwrap().success());
    let mut rasters: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("raster-")).collect();
    rasters.sort();
    let bytes: Vec<Vec<u8>> = rasters.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let out = pdf::image_only_pdf(&bytes, &Deadline::NONE).unwrap();
    for forbidden in [&b"/JavaScript"[..], b"/OpenAction", b"/EmbeddedFile", b"MALICIOUS_MARKER"] {
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
        rasters.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        dir.join("ref.pdf").display()
    );
    if Command::new("python3").args(["-c", &py]).status().map(|s| s.success()).unwrap_or(false) {
        assert_eq!(std::fs::read(dir.join("ref.pdf")).unwrap(), out, "PDF differs from the Python writer");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pdfinfo_rules() {
    assert_eq!(pdf::pages_from_pdfinfo(b"Pages:           3\nEncrypted:       no\n").unwrap(), 3);
    assert!(pdf::pages_from_pdfinfo(b"Pages:           33\nEncrypted:       no\n").is_err());
    assert!(pdf::pages_from_pdfinfo(b"Pages:           1\nEncrypted:       yes (print:yes)\n").is_err());
    assert!(pdf::pages_from_pdfinfo(b"Encrypted:       no\n").is_err());
    assert!(pdf::pages_from_pdfinfo(&vec![b'a'; 70_000]).is_err());
}
