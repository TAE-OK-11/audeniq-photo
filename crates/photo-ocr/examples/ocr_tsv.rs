//! `ocr_tsv <image.png>`: Tesseract-style `--psm 11 -l eng+kor tsv` output.
use photo_ocr::{Model, Pix, models};

fn main() {
    let path = std::env::args().nth(1).expect("image path");
    let data = std::fs::read(&path).expect("image");
    let (_, img) = photo_png::decode(
        &data,
        &photo_core::Limits::default(),
        &photo_core::Deadline::NONE,
    )
    .expect("png");
    let pix = Pix::from_image(&img).expect("not CMYK");
    let eng = Model::load(models::ENG).expect("eng");
    let kor = Model::load(models::KOR).expect("kor");
    print!("{}", photo_ocr::ocr_tsv(&pix, &[&eng, &kor]));
}
