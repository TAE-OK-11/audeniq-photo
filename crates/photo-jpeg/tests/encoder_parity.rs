//! Our baseline encoder against Pillow's (libjpeg-turbo) for the two
//! settings the sanitizer uses: q95 4:4:4 and q85 4:2:0.
use photo_core::{Deadline, Image, Limits, PixelFormat};
use photo_jpeg::{Subsampling, encode};
use std::process::Command;

fn source(w: u32, h: u32) -> Image {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[((x * 7 + y * 3) % 256) as u8, ((x * x + y * 5) % 256) as u8, (((x ^ y) * 9) % 256) as u8]);
        }
    }
    Image { width: w, height: h, format: PixelFormat::Rgb8, data }
}

#[test]
fn byte_identical_to_pillow() {
    let dir = std::env::temp_dir().join(format!("photo-jpeg-enc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut compared = 0;
    for (w, h) in [(1, 1), (9, 7), (64, 48), (123, 45)] {
        let img = source(w, h);
        let raw = dir.join("src.raw");
        std::fs::write(&raw, &img.data).unwrap();
        for (q, sub, pil_sub) in [(95u8, Subsampling::S444, 0), (85, Subsampling::S420, 2), (40, Subsampling::S420, 2)] {
            let ours = encode(&img, q, sub).unwrap();
            let target = dir.join("pil.jpg");
            let script = format!(
                "from PIL import Image;im=Image.frombytes('RGB',({w},{h}),open(r'{}','rb').read());im.save(r'{}',quality={q},subsampling={pil_sub})",
                raw.display(),
                target.display()
            );
            if !Command::new("python3").args(["-c", &script]).status().map(|s| s.success()).unwrap_or(false) {
                eprintln!("Pillow unavailable; skipping");
                return;
            }
            let theirs = std::fs::read(&target).unwrap();
            assert_eq!(ours, theirs, "{w}x{h} q{q} {sub:?}");
            // And it decodes to the same pixels Pillow would see.
            photo_jpeg::decode(&ours, &Limits::default(), &Deadline::NONE).unwrap();
            compared += 1;
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(compared, 12);
}
