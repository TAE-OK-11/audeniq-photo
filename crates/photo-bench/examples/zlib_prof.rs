//! zlib engine timing on image-like data (development aid).
fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let img = audeniq_photo::sanitize::pixels(&data, &audeniq_photo::Deadline::NONE).unwrap();
    let mut best = (f64::MAX, f64::MAX, f64::MAX);
    let mut size = 0;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let z = photo_deflate::compress_zlib(&img.data, photo_deflate::Level::DEFAULT);
        let c = t.elapsed().as_secs_f64() * 1000.0;
        size = z.len();
        let t = std::time::Instant::now();
        let mut out = Vec::new();
        photo_deflate::inflate_zlib(&z, &mut out, img.data.len(), false).unwrap();
        let d = t.elapsed().as_secs_f64() * 1000.0;
        let t = std::time::Instant::now();
        std::hint::black_box(photo_deflate::crc32(&img.data));
        let k = t.elapsed().as_secs_f64() * 1000.0;
        best = (best.0.min(c), best.1.min(d), best.2.min(k));
    }
    println!(
        "deflate6 {:.1} ms ({} KiB)  inflate {:.1} ms  crc32 {:.2} ms",
        best.0,
        size / 1024,
        best.1,
        best.2
    );
}
