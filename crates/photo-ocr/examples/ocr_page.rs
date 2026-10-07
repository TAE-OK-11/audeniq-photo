//! `ocr_page <image.png> <dump dir>`: run the page pipeline and dump each
//! stage for comparison with an instrumented Tesseract build.
use photo_ocr::{Pix, page};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0]).expect("image");
    let (_, img) = photo_png::decode(
        &data,
        &photo_core::Limits::default(),
        &photo_core::Deadline::NONE,
    )
    .expect("png");
    let pix = Pix::from_image(&img).expect("not CMYK");
    let dir = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(dir).unwrap();
    let mut bin = page::thresh::threshold(&pix);
    std::fs::write(dir.join("01_binary.pbm"), bin.to_pbm()).unwrap();
    let lines = page::linefind::find_and_remove_lines(70, &mut bin);
    eprintln!(
        "v_lines={} h_lines={} vertical=({}, {})",
        lines.v_lines.len(),
        lines.h_lines.len(),
        lines.vertical_x,
        lines.vertical_y
    );
    std::fs::write(dir.join("02_nolines.pbm"), bin.to_pbm()).unwrap();
    let photo = page::imagefind::find_images(&bin);
    std::fs::write(dir.join("03_photomask.pbm"), photo.to_pbm()).unwrap();
    let mut blobs = page::blobbox::Blobs::default();
    let tb = page::blobbox::find_components(&bin, &mut blobs).expect("size");
    let mut out = format!(
        "line_size {} line_spacing {}\n",
        fmt_g(f64::from(tb.line_size)),
        fmt_g(f64::from(tb.line_spacing))
    );
    for (name, list) in [
        ("blobs", &tb.blobs),
        ("small", &tb.small_blobs),
        ("noise", &tb.noise_blobs),
        ("large", &tb.large_blobs),
    ] {
        for id in list.to_vec() {
            let b = blobs.get(id).bbox;
            out += &format!("{name} {} {} {} {}\n", b.left, b.bottom, b.right, b.top);
        }
    }
    std::fs::write(dir.join("04_blobs.txt"), out).unwrap();
}

/// C's `%g`.
fn fmt_g(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let exp = v.abs().log10().floor() as i32;
    if !(-4..6).contains(&exp) {
        let s = format!("{:.5e}", v);
        let (m, e) = s.split_once('e').unwrap();
        let m = m.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().unwrap();
        return format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let prec = (5 - exp).max(0) as usize;
    let s = format!("{:.*}", prec, v);
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}
