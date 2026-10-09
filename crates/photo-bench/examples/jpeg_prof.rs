use audeniq_photo::{Deadline, Limits};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(path).unwrap();
    let mode = std::env::args().nth(2).unwrap_or_default();
    let (_, img) = photo_jpeg::decode(&data, &Limits::default(), &Deadline::NONE).unwrap();
    let img = img.into_rgb8();
    let mut best = f64::MAX;
    for _ in 0..10 {
        let t = std::time::Instant::now();
        if mode == "enc" {
            std::hint::black_box(
                photo_jpeg::encode(&img, 95, photo_jpeg::Subsampling::S444).unwrap(),
            );
        } else {
            std::hint::black_box(
                photo_jpeg::decode(&data, &Limits::default(), &Deadline::NONE).unwrap(),
            );
        }
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("{mode} {best:.1} ms (best of 10)");
}
