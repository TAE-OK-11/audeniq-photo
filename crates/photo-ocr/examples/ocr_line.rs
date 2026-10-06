//! `ocr_line <model.traineddata> <image.png|jpg>...`: recognize each image as
//! one text line (like `tesseract --psm 13`) and print `conf\ttext` per word.
use photo_ocr::{Model, OcrRandom, Pix};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let model = Model::load(&std::fs::read(&args[0]).expect("model")).expect("load");
    for path in &args[1..] {
        let data = std::fs::read(path).expect("image");
        let img = audeniq_photo::decode_image(
            &data,
            &photo_core::Limits::default(),
            &photo_core::Deadline::NONE,
        )
        .expect("decode")
        .image;
        let gray = to_pix(&img);
        let words = model.recognize_line(&gray, &mut OcrRandom::default());
        println!("== {path}");
        for w in words {
            println!("{:.6}\t{}", w.word.confidence, w.word.text);
        }
    }
}

fn to_pix(img: &photo_core::Image) -> Pix {
    Pix::from_image(img).expect("CMYK not handled here")
}
