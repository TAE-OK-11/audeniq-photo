//! Timing breakdown of the page OCR stages (dev tool).
use photo_ocr::{Model, Pix, models};
use std::time::Instant;

fn main() {
    let t = Instant::now();
    let eng = Model::load(models::ENG).unwrap();
    let kor = Model::load(models::KOR).unwrap();
    eprintln!("load {:?}", t.elapsed());
    let (mut tl, mut tr) = (0.0, 0.0);
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).unwrap();
        let (_, img) = photo_png::decode(
            &data,
            &photo_core::Limits::default(),
            &photo_core::Deadline::NONE,
        )
        .unwrap();
        let pix = Pix::from_image(&img).unwrap();
        let t = Instant::now();
        let blocks = photo_ocr::page_blocks(&pix);
        tl += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let _ = photo_ocr::recog::recognize_page(&pix, &blocks, &[&eng, &kor], &|| false);
        tr += t.elapsed().as_secs_f64();
    }
    eprintln!("layout {tl:.2}s recog {tr:.2}s");
}
