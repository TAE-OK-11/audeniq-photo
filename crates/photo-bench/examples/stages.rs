//! Per-stage timing of the hot paths (development aid).
use audeniq_photo::{Deadline, Image, Limits, PixelFormat};
use std::time::Instant;

fn synthetic(w: u32, h: u32) -> Image {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    let mut x32 = 0x9E37_79B9u32;
    for y in 0..h {
        for x in 0..w {
            x32 ^= x32 << 13;
            x32 ^= x32 >> 17;
            x32 ^= x32 << 5;
            let n = (x32 & 15) as i32 - 8;
            data.extend_from_slice(&[
                ((x * 255 / w) as i32 + n).clamp(0, 255) as u8,
                ((y * 255 / h) as i32 + n).clamp(0, 255) as u8,
                ((((x / 37) ^ (y / 53)) & 0x3F) as i32 * 3 + 60 + n).clamp(0, 255) as u8,
            ]);
        }
    }
    Image {
        width: w,
        height: h,
        format: PixelFormat::Rgb8,
        data,
    }
}

fn t<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t0 = Instant::now();
    let r = f();
    println!(
        "{label:<32} {:>8.1} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    r
}

fn main() {
    let img = synthetic(3000, 3000);
    let jpg420 = t("jpeg encode q92 4:2:0", || {
        photo_jpeg::encode(&img, 92, photo_jpeg::Subsampling::S420).unwrap()
    });
    let _ = t("jpeg encode q95 4:4:4", || {
        photo_jpeg::encode(&img, 95, photo_jpeg::Subsampling::S444).unwrap()
    });
    let (_, dec) = t("jpeg decode 4:2:0", || {
        photo_jpeg::decode(&jpg420, &Limits::default(), &Deadline::NONE).unwrap()
    });
    let png = t("png encode level 6", || {
        photo_png::encode(&img, photo_deflate::Level::DEFAULT).unwrap()
    });
    for l in [1u8, 3, 4] {
        let p = t(&format!("png encode level {l}"), || {
            photo_png::encode(&img, photo_deflate::Level::new(l)).unwrap()
        });
        println!("    size level {l}: {} KiB", p.len() / 1024);
    }
    println!("    size level 6: {} KiB", png.len() / 1024);
    let raw: Vec<u8> = img.data.clone();
    let z = t("zlib level 6 (raw rgb)", || {
        photo_deflate::compress_zlib(&raw, photo_deflate::Level::DEFAULT)
    });
    let _ = t("inflate", || {
        let mut o = Vec::with_capacity(raw.len());
        photo_deflate::inflate_zlib(&z, &mut o, raw.len(), false).unwrap();
        o
    });
    let _ = t("png decode", || {
        photo_png::decode(&png, &Limits::default(), &Deadline::NONE).unwrap()
    });
    let gray = t("to_gray", || photo_qr::to_gray(&dec.data, 3));
    let _ = t("qr scan (threshold+identify)", || {
        photo_qr::scan(&gray, 3000, 3000, &Deadline::NONE).unwrap()
    });
    let _ = t("sanitize jpeg", || {
        audeniq_photo::sanitize(&jpg420, audeniq_photo::Kind::Jpeg, &Deadline::NONE).unwrap()
    });
}
