//! Whole-page OCR (`tesseract <img> stdout -l eng+kor --psm 11 tsv`)
//! compared byte for byte with Tesseract 5.3.4's TSV on generated covers:
//! layout, word boxes, text and confidences.
use photo_ocr::{Model, Pix, models};
use std::path::Path;

#[test]
fn page_tsv_matches_tesseract() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/page");
    let eng = Model::load(models::ENG).unwrap();
    let kor = Model::load(models::KOR).unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "png").then(|| p.file_stem().unwrap().to_owned())
        })
        .collect();
    names.sort();
    assert!(!names.is_empty());
    for name in names {
        let png = std::fs::read(dir.join(&name).with_extension("png")).unwrap();
        let (_, img) = photo_png::decode(
            &png,
            &photo_core::Limits::default(),
            &photo_core::Deadline::NONE,
        )
        .unwrap();
        let pix = Pix::from_image(&img).unwrap();
        let expected = std::fs::read_to_string(dir.join(&name).with_extension("tsv")).unwrap();
        let got = photo_ocr::ocr_tsv(&pix, &[&eng, &kor]);
        assert_eq!(got, expected, "{name:?}");
    }
}
