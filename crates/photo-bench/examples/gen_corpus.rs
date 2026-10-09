//! Write a small benchmark corpus of photo-like and graphic test images
//! (development aid): `gen_corpus OUT_DIR`.
//!
//! - `photo_*`: 1/f ("natural image") noise from summed bilinear octaves
//!   plus sensor-like grain, the statistics of photographs.
//! - `flat_*`: graphic-design covers (flat fills, gradients, stripes, text-
//!   like blocks), where LZ77 matches matter.
use audeniq_photo::{Image, PixelFormat};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn unit(&mut self) -> f32 {
        self.next() as f32 / (1u64 << 31) as f32
    }
}

fn photo(w: usize, h: usize, seed: u64, grain: f32) -> Image {
    let mut rng = Rng(seed);
    let mut acc = vec![[0f32; 3]; w * h];
    let mut cell = w.max(h) / 2;
    let mut amp = 1.0f32;
    while cell >= 2 {
        let gw = w / cell + 2;
        let gh = h / cell + 2;
        let grid: Vec<[f32; 3]> = (0..gw * gh)
            .map(|_| {
                let l = rng.unit() - 0.5;
                [
                    l + 0.3 * (rng.unit() - 0.5),
                    l + 0.3 * (rng.unit() - 0.5),
                    l + 0.3 * (rng.unit() - 0.5),
                ]
            })
            .collect();
        for y in 0..h {
            let fy = y as f32 / cell as f32;
            let (y0, ty) = (fy as usize, fy.fract());
            for x in 0..w {
                let fx = x as f32 / cell as f32;
                let (x0, tx) = (fx as usize, fx.fract());
                let g = |xx: usize, yy: usize| grid[yy * gw + xx];
                let (a, b, c, d) = (g(x0, y0), g(x0 + 1, y0), g(x0, y0 + 1), g(x0 + 1, y0 + 1));
                for k in 0..3 {
                    let top = a[k] + (b[k] - a[k]) * tx;
                    let bot = c[k] + (d[k] - c[k]) * tx;
                    acc[y * w + x][k] += amp * (top + (bot - top) * ty);
                }
            }
        }
        cell /= 2;
        amp *= 0.62;
    }
    let mut data = Vec::with_capacity(w * h * 3);
    for p in &acc {
        for &c in p {
            let v = 128.0 + 150.0 * c + grain * (rng.unit() - 0.5);
            data.push(v.clamp(0.0, 255.0) as u8);
        }
    }
    Image {
        width: w as u32,
        height: h as u32,
        format: PixelFormat::Rgb8,
        data,
    }
}

fn flat(w: usize, h: usize, seed: u64) -> Image {
    let mut rng = Rng(seed);
    let mut data = vec![0u8; w * h * 3];
    // Background gradient.
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            data[i] = (40 + y * 120 / h) as u8;
            data[i + 1] = (20 + x * 60 / w) as u8;
            data[i + 2] = 90;
        }
    }
    // Flat rectangles, stripes and "text" blocks.
    for _ in 0..60 {
        let (x0, y0) = (rng.next() as usize % w, rng.next() as usize % h);
        let (rw, rh) = (
            1 + rng.next() as usize % (w / 3),
            1 + rng.next() as usize % (h / 6),
        );
        let col = [rng.next() as u8, rng.next() as u8, rng.next() as u8];
        let kind = rng.next() % 3;
        for y in y0..(y0 + rh).min(h) {
            for x in x0..(x0 + rw).min(w) {
                let on = match kind {
                    0 => true,
                    1 => (x / 8 + y / 8) % 2 == 0,
                    _ => (x % 14 < 9) && (y % 22 < 15) && ((x / 14 * 7 + y / 22 * 3) % 5 != 0),
                };
                if on {
                    let i = (y * w + x) * 3;
                    data[i..i + 3].copy_from_slice(&col);
                }
            }
        }
    }
    Image {
        width: w as u32,
        height: h as u32,
        format: PixelFormat::Rgb8,
        data,
    }
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("OUT_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let set: Vec<(&str, Image)> = vec![
        ("photo_3000", photo(3000, 3000, 1, 6.0)),
        ("photo_1400", photo(1400, 1400, 2, 10.0)),
        ("photo_clean_3000", photo(3000, 3000, 3, 1.0)),
        ("flat_3000", flat(3000, 3000, 4)),
        ("flat_1400", flat(1400, 1400, 5)),
    ];
    for (name, img) in &set {
        let png = audeniq_photo::sanitize::encode_png(img).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), &png).unwrap();
        let jpg = audeniq_photo::sanitize::encode_jpeg(img, 92).unwrap();
        std::fs::write(dir.join(format!("{name}.jpg")), &jpg).unwrap();
        println!(
            "{name}: png {} KiB, jpg {} KiB",
            png.len() / 1024,
            jpg.len() / 1024
        );
    }
}
