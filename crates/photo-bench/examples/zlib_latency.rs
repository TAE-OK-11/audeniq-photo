//! Large- and small-stream zlib latency (development aid).
use std::time::Instant;
fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let img = audeniq_photo::sanitize::pixels(&data, &audeniq_photo::Deadline::NONE).unwrap();
    let z = photo_deflate::compress_zlib(&img.data, photo_deflate::Level::DEFAULT);
    let mut big_c = f64::MAX;
    let mut big_d = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        std::hint::black_box(photo_deflate::compress_zlib(
            &img.data,
            photo_deflate::Level::DEFAULT,
        ));
        big_c = big_c.min(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let mut out = Vec::with_capacity(img.data.len());
        photo_deflate::inflate_zlib(&z, &mut out, img.data.len(), false).unwrap();
        big_d = big_d.min(t.elapsed().as_secs_f64() * 1e3);
    }
    // Small streams: like PNG text/iCCP chunks and signature IDAT.
    let small: Vec<u8> = img.data[..3000].to_vec();
    let zs = photo_deflate::compress_zlib(&small, photo_deflate::Level::DEFAULT);
    let n = 20_000;
    let t = Instant::now();
    for _ in 0..n {
        let mut out = Vec::new();
        photo_deflate::inflate_zlib(&zs, &mut out, 1 << 20, false).unwrap();
        std::hint::black_box(out);
    }
    let small_d = t.elapsed().as_secs_f64() * 1e6 / n as f64;
    let t = Instant::now();
    for _ in 0..n / 4 {
        std::hint::black_box(photo_deflate::compress_zlib(
            &small,
            photo_deflate::Level::DEFAULT,
        ));
    }
    let small_c = t.elapsed().as_secs_f64() * 1e6 / (n / 4) as f64;
    println!(
        "big: deflate {big_c:.1} ms, inflate {big_d:.1} ms | small 3 KB: inflate {small_d:.1} µs, deflate {small_c:.1} µs"
    );
}
