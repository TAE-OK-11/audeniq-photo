//! PNG size/time per zlib level on an input image (development aid).
use audeniq_photo::Deadline;
fn main() {
    let path = std::env::args().nth(1).expect("image path");
    let data = std::fs::read(path).unwrap();
    let img = audeniq_photo::sanitize::pixels(&data, &Deadline::NONE).unwrap();
    for level in 1..=9u8 {
        let t = std::time::Instant::now();
        let png = photo_png::encode(&img, photo_deflate::Level::new(level)).unwrap();
        println!(
            "level {level}: {:>6} KiB {:>7.1} ms",
            png.len() / 1024,
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
}
