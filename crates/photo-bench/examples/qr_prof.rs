use audeniq_photo::{Deadline, Limits};
fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let (_, img) = photo_jpeg::decode_luma(&data, &Limits::default(), &Deadline::NONE).unwrap();
    let t = std::time::Instant::now();
    for _ in 0..10 {
        std::hint::black_box(photo_qr::scan(&img.data, img.width as usize, img.height as usize, &Deadline::NONE).unwrap());
    }
    println!("qr {:.1} ms/iter", t.elapsed().as_secs_f64() * 100.0);
}
