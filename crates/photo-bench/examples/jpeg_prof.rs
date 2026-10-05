use audeniq_photo::{Deadline, Limits};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(path).unwrap();
    let mode = std::env::args().nth(2).unwrap_or_default();
    let (_, img) = photo_jpeg::decode(&data, &Limits::default(), &Deadline::NONE).unwrap();
    let img = img.into_rgb8();
    let t = std::time::Instant::now();
    for _ in 0..10 {
        if mode == "enc" {
            std::hint::black_box(
                photo_jpeg::encode(&img, 95, photo_jpeg::Subsampling::S444).unwrap(),
            );
        } else {
            std::hint::black_box(
                photo_jpeg::decode(&data, &Limits::default(), &Deadline::NONE).unwrap(),
            );
        }
    }
    println!("{mode} {:.1} ms/iter", t.elapsed().as_secs_f64() * 100.0);
}
