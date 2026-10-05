//! Port of deploy/tests/test_upload_sanitizer.py.
use audeniq_photo::{Deadline, Kind, PixelFormat, sanitize};

fn png_rgb(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let img = audeniq_photo::Image {
        width: w,
        height: h,
        format: PixelFormat::Rgb8,
        data: rgb.repeat((w * h) as usize),
    };
    photo_png::encode(&img, photo_deflate::Level::DEFAULT).unwrap()
}

fn insert_chunk(png: &[u8], kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = png[..33].to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut c = photo_deflate::Crc32::new();
    c.update(kind);
    c.update(body);
    out.extend_from_slice(&c.finish().to_be_bytes());
    out.extend_from_slice(&png[33..]);
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn png_metadata_and_appended_payload_are_removed() {
    let mut src = insert_chunk(
        &png_rgb(32, 32, [255, 0, 0]),
        b"tEXt",
        b"Script\0MALICIOUS_MARKER",
    );
    src.extend_from_slice(b"<script>MALICIOUS_MARKER</script>");
    let out = sanitize(&src, Kind::Png, &Deadline::NONE).unwrap();
    assert!(!contains(&out, b"MALICIOUS_MARKER"));
    let (info, img) = photo_png::decode(&out, &Default::default(), &Deadline::NONE).unwrap();
    assert_eq!((img.width, img.height), (32, 32));
    assert_eq!(&img.data[..3], &[255, 0, 0]);
    assert!(info.texts.is_empty() && info.icc_profile.is_none() && info.exif.is_none());
}

#[test]
fn animated_and_oversized_images_are_rejected() {
    // APNG with acTL num_frames = 2.
    let mut actl = Vec::new();
    actl.extend_from_slice(&2u32.to_be_bytes());
    actl.extend_from_slice(&0u32.to_be_bytes());
    let apng = insert_chunk(&png_rgb(4, 4, [255, 0, 0]), b"acTL", &actl);
    assert!(sanitize(&apng, Kind::Png, &Deadline::NONE).is_err());
    assert!(sanitize(&png_rgb(8001, 1, [0, 0, 0]), Kind::Png, &Deadline::NONE).is_err());
}

#[test]
fn html_and_archives_are_never_accepted() {
    let src = b"<script>alert(1)</script>";
    for mime in [
        "text/html",
        "image/svg+xml",
        "application/zip",
        "application/msword",
    ] {
        assert!(Kind::from_mime(mime).is_none());
    }
    assert!(sanitize(src, Kind::Png, &Deadline::NONE).is_err());
    assert!(sanitize(src, Kind::Jpeg, &Deadline::NONE).is_err());
}

#[test]
fn signature_crc_and_exact_pixels_and_no_hidden_metadata() {
    let original = png_rgb(32, 10, [255, 255, 255]);
    assert!(sanitize(&original, Kind::Signature, &Deadline::NONE).is_ok());
    let mut appended = original.clone();
    appended.extend_from_slice(b"hidden executable");
    assert!(sanitize(&appended, Kind::Signature, &Deadline::NONE).is_err());
    let mut flipped = original.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert!(sanitize(&flipped, Kind::Signature, &Deadline::NONE).is_err());
    let with_text = insert_chunk(&original, b"tEXt", b"Script\0MALICIOUS_MARKER");
    assert!(sanitize(&with_text, Kind::Signature, &Deadline::NONE).is_err());
}

#[test]
fn hostile_inputs_never_panic() {
    // Mutations of valid files must fail cleanly or succeed, never panic.
    let png = png_rgb(40, 30, [10, 200, 30]);
    let img = audeniq_photo::Image {
        width: 40,
        height: 30,
        format: PixelFormat::Rgb8,
        data: (0..3600u32).map(|i| (i * 7) as u8).collect(),
    };
    let jpg = photo_jpeg::encode(&img, 80, photo_jpeg::Subsampling::S420).unwrap();
    let mut x = 0x1234_5678u32;
    for base in [&png, &jpg] {
        for _ in 0..1500 {
            let mut m = base.clone();
            for _ in 0..1 + (x % 4) {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = (x as usize >> 8) % m.len();
                m[i] = (x >> 3) as u8;
            }
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if x.is_multiple_of(7) {
                let cut = (x as usize >> 4) % m.len();
                m.truncate(cut);
            }
            for kind in [Kind::Png, Kind::Jpeg] {
                let r = sanitize(&m, kind, &Deadline::NONE);
                assert!(
                    !matches!(r, Err(audeniq_photo::Error::Internal)),
                    "decoder panicked"
                );
            }
            let _ = audeniq_photo::metadata(&m);
            let _ = audeniq_photo::probe(&m);
            let _ = audeniq_photo::qr_count(&m, &Deadline::NONE);
        }
    }
}

#[test]
fn verify_image_rejects_corrupt_pixel_data_with_valid_headers() {
    let png = png_rgb(64, 64, [1, 2, 3]);
    assert_eq!(
        audeniq_photo::verify_image(&png, &Deadline::NONE)
            .unwrap()
            .width,
        64
    );
    // Damage the IDAT payload but keep chunk CRCs consistent? Simplest:
    // truncate after the header so the probe passes and the decode fails.
    let mut cut = png[..png.len() - 20].to_vec();
    cut.extend_from_slice(&png[png.len() - 12..]);
    assert!(
        audeniq_photo::probe(&cut).is_err()
            || audeniq_photo::verify_image(&cut, &Deadline::NONE).is_err()
    );
    // Large enough that cutting the file in half lands in entropy data.
    let img = audeniq_photo::Image {
        width: 256,
        height: 256,
        format: PixelFormat::Rgb8,
        data: (0..256 * 256 * 3u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect(),
    };
    let jpg = photo_jpeg::encode(&img, 90, photo_jpeg::Subsampling::S420).unwrap();
    assert!(audeniq_photo::verify_image(&jpg, &Deadline::NONE).is_ok());
    let truncated = &jpg[..jpg.len() / 2];
    assert!(audeniq_photo::probe(truncated).is_ok());
    assert!(matches!(
        audeniq_photo::verify_image(truncated, &Deadline::NONE),
        Err(audeniq_photo::Error::Invalid(_))
    ));
}
