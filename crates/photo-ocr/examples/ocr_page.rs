//! `ocr_page <image.png> <dump dir>`: run the page pipeline and dump each
//! stage for comparison with an instrumented Tesseract build.
use photo_ocr::{Pix, page};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0]).expect("image");
    let (_, img) = photo_png::decode(
        &data,
        &photo_core::Limits::default(),
        &photo_core::Deadline::NONE,
    )
    .expect("png");
    let pix = Pix::from_image(&img).expect("not CMYK");
    let dir = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(dir).unwrap();
    let bin = page::thresh::threshold(&pix);
    std::fs::write(dir.join("01_binary.pbm"), bin.to_pbm()).unwrap();
    let masks = page::linefind::line_masks(70, &bin);
    eprintln!(
        "vline={} hline={}",
        masks.vline.is_some(),
        masks.hline.is_some()
    );
    let photo = page::imagefind::find_images(&bin);
    std::fs::write(dir.join("03_photomask.pbm"), photo.to_pbm()).unwrap();
}
