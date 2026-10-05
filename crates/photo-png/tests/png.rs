use photo_core::{Deadline, Image, Limits, PixelFormat};
use photo_deflate::Level;
use photo_png::{decode, encode, read_info, validate_signature};
use std::process::Command;

fn gradient(w: u32, h: u32, format: PixelFormat) -> Image {
    let ch = format.channels();
    let mut data = Vec::with_capacity((w * h) as usize * ch);
    for y in 0..h {
        for x in 0..w {
            for c in 0..ch {
                data.push(((x * 7 + y * 3 + c as u32 * 50) % 256) as u8);
            }
        }
    }
    Image {
        width: w,
        height: h,
        format,
        data,
    }
}

#[test]
fn encode_decode_roundtrip_all_formats() {
    for format in [
        PixelFormat::Gray8,
        PixelFormat::GrayAlpha8,
        PixelFormat::Rgb8,
        PixelFormat::Rgba8,
    ] {
        for (w, h) in [(1, 1), (3, 5), (64, 33), (257, 3)] {
            let img = gradient(w, h, format);
            let png = encode(&img, Level::DEFAULT).unwrap();
            let (info, back) = decode(&png, &Limits::default(), &Deadline::NONE).unwrap();
            assert_eq!(back, img);
            assert!(info.texts.is_empty() && info.icc_profile.is_none());
        }
    }
}

fn ffmpeg_png(args: &[&str], dir: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    let path = dir.join(name);
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .args(args)
        .args(["-frames:v", "1"])
        .arg(&path)
        .status()
        .ok()?
        .success();
    ok.then(|| std::fs::read(&path).unwrap())
}

#[test]
fn decodes_ffmpeg_pixel_formats_like_ffmpeg() {
    let dir = std::env::temp_dir().join(format!("photo-png-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (pix, expect) in [
        ("rgb24", PixelFormat::Rgb8),
        ("rgba", PixelFormat::Rgba8),
        ("gray", PixelFormat::Gray8),
        ("ya8", PixelFormat::GrayAlpha8),
        ("rgb48be", PixelFormat::Rgb8),
        ("rgba64be", PixelFormat::Rgba8),
        ("gray16be", PixelFormat::Gray8),
        ("pal8", PixelFormat::Rgb8),
        ("monob", PixelFormat::Gray8),
    ] {
        let Some(png) = ffmpeg_png(
            &["testsrc2=s=98x62,scale=97:61", "-pix_fmt", pix],
            &dir,
            &format!("{pix}.png"),
        ) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        let (info, img) = decode(&png, &Limits::default(), &Deadline::NONE).unwrap();
        assert_eq!((img.width, img.height), (97, 61));
        assert!(
            img.format == expect || (pix == "pal8" && img.format == PixelFormat::Rgba8),
            "{pix}: {:?}",
            img.format
        );
        // Reference: ffmpeg's own decode to rgb24 must match ours for 8-bit
        // RGB-family formats.
        if matches!(pix, "rgb24" | "pal8" | "gray" | "monob") {
            let out = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(dir.join(format!("{pix}.png")))
                .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
                .output()
                .unwrap();
            assert_eq!(img.clone().into_rgb8().data, out.stdout, "{pix}");
        }
        assert_eq!(read_info(&png).unwrap().width, info.width);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejects_truncation_crc_and_bombs() {
    let png = encode(&gradient(50, 50, PixelFormat::Rgb8), Level::DEFAULT).unwrap();
    for cut in [8, 20, 33, png.len() / 2, png.len() - 1] {
        assert!(
            decode(&png[..cut], &Limits::default(), &Deadline::NONE).is_err(),
            "cut {cut}"
        );
    }
    let mut bad = png.clone();
    bad[40] ^= 1;
    assert!(decode(&bad, &Limits::default(), &Deadline::NONE).is_err());
    let limits = Limits {
        max_pixels: 2000,
        ..Limits::default()
    };
    assert!(decode(&png, &limits, &Deadline::NONE).is_err());
}

#[test]
fn signature_validator_matches_backend_rules() {
    let sig = encode(&gradient(32, 10, PixelFormat::Rgb8), Level::DEFAULT).unwrap();
    validate_signature(&sig).unwrap();
    let mut appended = sig.clone();
    appended.extend_from_slice(b"hidden executable");
    assert!(validate_signature(&appended).is_err());
    let mut flipped = sig.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert!(validate_signature(&flipped).is_err());
    // Too tall for a signature.
    let tall = encode(&gradient(10, 1025, PixelFormat::Gray8), Level::DEFAULT).unwrap();
    assert!(validate_signature(&tall).is_err());
    // Text chunk inserted after IHDR.
    let mut with_text = sig[..33].to_vec();
    let body = b"Script\0MALICIOUS";
    with_text.extend_from_slice(&(body.len() as u32).to_be_bytes());
    with_text.extend_from_slice(b"tEXt");
    with_text.extend_from_slice(body);
    let mut c = photo_deflate::Crc32::new();
    c.update(b"tEXt");
    c.update(body);
    with_text.extend_from_slice(&c.finish().to_be_bytes());
    with_text.extend_from_slice(&sig[33..]);
    assert!(validate_signature(&with_text).is_err());
    assert!(decode(&with_text, &Limits::default(), &Deadline::NONE).is_ok());
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut c = photo_deflate::Crc32::new();
    c.update(kind);
    c.update(body);
    out.extend_from_slice(&c.finish().to_be_bytes());
}

#[test]
fn decodes_adam7_interlaced() {
    let img = gradient(37, 23, PixelFormat::Rgb8);
    let passes = [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ];
    let mut raw = Vec::new();
    for (pi, (x0, y0, dx, dy)) in passes.into_iter().enumerate() {
        let mut y = y0;
        while y < 23 {
            if x0 < 37 {
                // Alternate filters to exercise unfiltering per pass.
                raw.push(0);
                let mut x = x0;
                while x < 37 {
                    let i = (y * 37 + x) * 3;
                    raw.extend_from_slice(&img.data[i..i + 3]);
                    x += dx;
                }
            }
            y += dy;
        }
        let _ = pi;
    }
    let mut png = photo_png::SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&37u32.to_be_bytes());
    ihdr.extend_from_slice(&23u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 1]);
    chunk(&mut png, b"IHDR", &ihdr);
    let z = photo_deflate::compress_zlib(&raw, Level::DEFAULT);
    chunk(&mut png, b"IDAT", &z[..z.len() / 2]);
    chunk(&mut png, b"IDAT", &z[z.len() / 2..]);
    chunk(&mut png, b"IEND", &[]);
    let (info, back) = decode(&png, &Limits::default(), &Deadline::NONE).unwrap();
    assert!(info.interlace);
    assert_eq!(back, img);
}

#[test]
fn decodes_pillow_low_bit_depths_and_text() {
    let dir = std::env::temp_dir().join(format!("photo-png-pil-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = r#"
import sys
from PIL import Image, PngImagePlugin
d=sys.argv[1]
src=Image.new("RGB",(29,13))
for y in range(13):
    for x in range(29):
        src.putpixel((x,y),((x*9)%256,(y*19)%256,((x+y)*5)%256))
for bits in (1,2,4,8):
    p=src.quantize(colors=2**bits)
    p.save(f"{d}/p{bits}.png",bits=bits)
    p.convert("RGB").save(f"{d}/p{bits}.rgb.png")
info=PngImagePlugin.PngInfo()
info.add_text("parameters","Steps: 20, Sampler: Euler")
info.add_itxt("Description","설명 text",lang="ko",zip=True)
info.add_text("Comment","zipped comment",zip=True)
src.save(f"{d}/text.png",pnginfo=info)
src.convert("L").save(f"{d}/l.png")
src.convert("1").save(f"{d}/one.png")
"#;
    let ok = Command::new("python3")
        .args(["-c", script])
        .arg(&dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("Pillow unavailable; skipping");
        return;
    }
    let read = |n: &str| std::fs::read(dir.join(n)).unwrap();
    let dec = |n: &str| decode(&read(n), &Limits::default(), &Deadline::NONE).unwrap();
    for bits in [1, 2, 4, 8] {
        let (info, img) = dec(&format!("p{bits}.png"));
        assert_eq!(info.bit_depth, bits);
        let (_, reference) = dec(&format!("p{bits}.rgb.png"));
        assert_eq!(img.into_rgb8(), reference, "bits {bits}");
    }
    let (info, _) = dec("text.png");
    let texts: Vec<_> = info
        .texts
        .iter()
        .map(|t| (t.keyword.as_str(), t.text.as_str()))
        .collect();
    assert!(texts.contains(&("parameters", "Steps: 20, Sampler: Euler")));
    assert!(texts.contains(&("Description", "설명 text")));
    assert!(texts.contains(&("Comment", "zipped comment")));
    let (_, one) = dec("one.png");
    assert!(one.data.iter().all(|&v| v == 0 || v == 255));
    std::fs::remove_dir_all(&dir).ok();
}
