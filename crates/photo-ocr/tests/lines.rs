//! Single-line recognition (`tesseract --psm 13`) compared with the output
//! of Tesseract 5.3.4 on the same images (`fixtures/expected.tsv`: word
//! text and confidence, which must match to the printed 6 decimals).
use photo_ocr::{Model, OcrRandom, Pix, models};
use std::path::Path;

fn load_png(path: &Path) -> Pix {
    let data = std::fs::read(path).unwrap();
    let (_, img) = photo_png::decode(
        &data,
        &photo_core::Limits::default(),
        &photo_core::Deadline::NONE,
    )
    .unwrap();
    Pix::from_image(&img).unwrap()
}

#[test]
fn lines_match_tesseract_word_for_word() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let expected = std::fs::read_to_string(dir.join("expected.tsv")).unwrap();
    let eng = Model::load(models::ENG).unwrap();
    let kor = Model::load(models::KOR).unwrap();
    let mut files: Vec<(String, String)> = Vec::new();
    for l in expected.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if !files.iter().any(|(n, _)| n == f[0]) {
            files.push((f[0].to_string(), f[1].to_string()));
        }
    }
    let mut ours = String::new();
    for (name, lang) in &files {
        let model = if lang == "kor" { &kor } else { &eng };
        for w in model.recognize_line(&load_png(&dir.join(name)), &mut OcrRandom::default()) {
            ours.push_str(&format!(
                "{name}\t{lang}\t{:.6}\t{}\n",
                w.word.confidence, w.word.text
            ));
        }
    }
    assert_eq!(ours, expected);
}

#[test]
fn degenerate_lines_match_tesseract() {
    // Tesseract hallucinates on blank lines; the port must do the same.
    let eng = Model::load(models::ENG).unwrap();
    let white = |w: usize, h: usize| {
        Pix::Gray(photo_ocr::Gray {
            width: w,
            height: h,
            data: vec![255; w * h],
        })
    };
    let run = |p: &Pix| -> Vec<(String, String)> {
        eng.recognize_line(p, &mut OcrRandom::default())
            .into_iter()
            .map(|w| (format!("{:.6}", w.word.confidence), w.word.text))
            .collect()
    };
    let s = |c: &str, t: &str| (c.to_string(), t.to_string());
    assert_eq!(run(&white(1, 1)), vec![s("23.005417", "_")]);
    assert_eq!(run(&white(500, 1)), vec![s("13.468353", "nn")]);
    assert_eq!(run(&white(300, 40)), vec![s("0.000000", "Be")]);
    // Too narrow after scaling: no input at all.
    assert!(run(&white(2, 40)).is_empty());
    assert!(run(&white(0, 0)).is_empty());
}

#[test]
fn corrupt_models_are_rejected() {
    assert!(Model::load(b"").is_err());
    assert!(Model::load(&models::ENG[..1000]).is_err());
    let mut bad = models::ENG.to_vec();
    for i in (0..bad.len()).step_by(997) {
        bad[i] ^= 0x5a;
    }
    let _ = Model::load(&bad); // must not panic
}
