//! Robustness sweep: synthetic and fixture images through the page OCR,
//! reporting panics (dev tool).
use photo_ocr::{Model, Pix, models};

fn rgb(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Pix {
    let mut d = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            d.extend_from_slice(&f(x, y));
        }
    }
    Pix::from_rgb(w, h, &d, 3)
}

fn main() {
    let eng = Model::load(models::ENG).unwrap();
    let kor = Model::load(models::KOR).unwrap();
    let mut cases: Vec<(String, Pix)> = Vec::new();
    let mut seed = 7u64;
    let mut rnd = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    for &(w, h) in &[
        (1, 1),
        (2, 2),
        (1, 300),
        (300, 1),
        (3, 500),
        (500, 3),
        (64, 64),
        (400, 300),
        (1200, 40),
    ] {
        for kind in 0..6 {
            let mut noise: Vec<u8> = (0..w * h).map(|_| rnd() as u8).collect();
            let thr = (rnd() % 256) as u8;
            noise
                .iter_mut()
                .for_each(|v| *v = if *v > thr { 255 } else { 0 });
            let pix = rgb(w, h, |x, y| match kind {
                0 => [0, 0, 0],
                1 => [255, 255, 255],
                2 => {
                    let v = noise[y * w + x];
                    [v, v, v]
                }
                3 => {
                    if (x / 3) % 2 == 0 {
                        [0, 0, 0]
                    } else {
                        [255; 3]
                    }
                }
                4 => {
                    if (y / 2 + x / 7) % 2 == 0 {
                        [10, 20, 200]
                    } else {
                        [250, 240, 10]
                    }
                }
                _ => {
                    if (x * 7 + y * 13) % 29 < 9 {
                        [0; 3]
                    } else {
                        [255; 3]
                    }
                }
            });
            cases.push((format!("{w}x{h}/k{kind}"), pix));
        }
    }
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).unwrap();
        if let Ok(d) = audeniq_photo::decode_image(
            &data,
            &photo_core::Limits::default(),
            &photo_core::Deadline::NONE,
        ) && let Some(p) = Pix::from_image(&d.image)
        {
            cases.push((path.clone(), p));
        }
    }
    let mut bad = 0;
    for (name, pix) in &cases {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            photo_ocr::ocr_tsv(pix, &[&eng, &kor])
        }));
        if r.is_err() {
            bad += 1;
            eprintln!("PANIC {name}");
        }
    }
    eprintln!("{} cases, {bad} panics", cases.len());
}
