//! The Rust sanitizer against the original `sanitize-upload.py` (kept in
//! tests/reference as the behavioural reference). Skips without Pillow.
use audeniq_photo::{Deadline, Kind, Limits, sanitize};
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURES: &str = r#"
import sys, io
from PIL import Image, ImageCms, PngImagePlugin
d = sys.argv[1]
def src(w, h):
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        for x in range(w):
            px[x, y] = ((x * 5 + y) % 256, (x * y) % 256, (255 - x * 3) % 256)
    return im
out = []
im = src(61, 37)
im.save(f"{d}/rgb.png"); out.append("rgb.png image/png")
im.convert("RGBA").save(f"{d}/rgba.png"); out.append("rgba.png image/png")
im.convert("L").save(f"{d}/gray.png"); out.append("gray.png image/png")
im.convert("LA").save(f"{d}/la.png"); out.append("la.png image/png")
im.quantize(16).save(f"{d}/pal.png"); out.append("pal.png image/png")
im.save(f"{d}/q90.jpg", quality=90); out.append("q90.jpg image/jpeg")
im.save(f"{d}/prog.jpg", quality=70, progressive=True); out.append("prog.jpg image/jpeg")
im.convert("L").save(f"{d}/gray.jpg"); out.append("gray.jpg image/jpeg")
im.convert("CMYK").save(f"{d}/cmyk.jpg"); out.append("cmyk.jpg image/jpeg")
for o in range(1, 9):
    exif = Image.Exif(); exif[0x0112] = o
    im.save(f"{d}/orient{o}.jpg", quality=92, exif=exif.tobytes()); out.append(f"orient{o}.jpg image/jpeg")
    im.save(f"{d}/orient{o}.png", exif=exif.tobytes()); out.append(f"orient{o}.png image/png")
adobe = "/usr/share/color/icc/compatibleWithAdobeRGB1998.icc"
cmykp = "/usr/share/color/icc/ghostscript/default_cmyk.icc"
grayp = "/usr/share/color/icc/ghostscript/sgray.icc"
import os
if os.path.exists(adobe):
    icc = open(adobe, "rb").read()
    im.save(f"{d}/adobe.jpg", quality=90, icc_profile=icc); out.append("adobe.jpg image/jpeg"); out.append("adobe.jpg image/png")
    im.save(f"{d}/adobe.png", icc_profile=icc); out.append("adobe.png image/png")
    im.convert("RGBA").save(f"{d}/adobe_rgba.png", icc_profile=icc); out.append("adobe_rgba.png image/png")
if os.path.exists(cmykp):
    im.convert("CMYK").save(f"{d}/cmyk_icc.jpg", icc_profile=open(cmykp, "rb").read()); out.append("cmyk_icc.jpg image/jpeg"); out.append("cmyk_icc.jpg image/png")
if os.path.exists(grayp):
    im.convert("L").save(f"{d}/gray_icc.jpg", icc_profile=open(grayp, "rb").read()); out.append("gray_icc.jpg image/jpeg")
sig = Image.new("RGB", (32, 10), "white")
sig.save(f"{d}/sig.png"); out.append("sig.png application/x-audeniq-signature")
Image.new("RGBA", (200, 80), (0, 0, 0, 0)).save(f"{d}/sig_rgba.png"); out.append("sig_rgba.png application/x-audeniq-signature")
print("\n".join(out))
"#;

fn python(args: &[&str]) -> Option<std::process::Output> {
    Command::new("python3").args(args).output().ok()
}

fn reference(script: &Path, src: &Path, dst: &Path, mime: &str) -> bool {
    Command::new("python3").arg(script).arg(src).arg(dst).arg(mime).status().map(|s| s.success()).unwrap_or(false)
}

fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let d = audeniq_photo::decode_image(bytes, &Limits::default(), &Deadline::NONE).unwrap();
    let img = d.image.into_rgb8();
    (img.width, img.height, img.data)
}

#[test]
fn matches_python_sanitizer() {
    let dir: PathBuf = std::env::temp_dir().join(format!("audeniq-photo-sanitize-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference/sanitize-upload.py");
    let Some(out) = python(&["-c", FIXTURES, dir.to_str().unwrap()]) else {
        eprintln!("python3 unavailable; skipping");
        return;
    };
    if !out.status.success() {
        eprintln!("Pillow unavailable; skipping");
        return;
    }
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for line in String::from_utf8(out.stdout).unwrap().lines() {
        let (name, mime) = line.split_once(' ').unwrap();
        let src = dir.join(name);
        let theirs_path = dir.join(format!("{name}.{}.ref", mime.replace('/', "_")));
        let theirs_ok = reference(&script, &src, &theirs_path, mime);
        let bytes = std::fs::read(&src).unwrap();
        let ours = sanitize(&bytes, Kind::from_mime(mime).unwrap(), &Deadline::NONE);
        match (theirs_ok, ours) {
            (false, Err(_)) => report.push(format!("{name}: both refused")),
            (true, Ok(ours)) => {
                let theirs = std::fs::read(&theirs_path).unwrap();
                let (tw, th, tp) = decode(&theirs);
                let (ow, oh, op) = decode(&ours);
                if (tw, th) != (ow, oh) {
                    failures.push(format!("{name}: size {tw}x{th} vs {ow}x{oh}"));
                    continue;
                }
                let max = tp.iter().zip(&op).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                let same_bytes = theirs == ours;
                report.push(format!("{name} -> {mime}: {ow}x{oh} max pixel diff {max} bytes {} ({} vs {} B)", if same_bytes { "identical" } else { "differ" }, ours.len(), theirs.len()));
                // ICC conversion is within ±3 of LittleCMS; a lossy JPEG
                // re-encode can spread those differences a little further.
                let icc = name.contains("icc") || name.contains("adobe");
                let allowed = match (icc, mime) {
                    (false, _) => 0,
                    (true, "image/jpeg") => 16,
                    (true, _) => 3,
                };
                if max > allowed {
                    failures.push(format!("{name}: max diff {max}"));
                }
            }
            (t, o) => failures.push(format!("{name}: reference ok={t} ours={:?}", o.map(|v| v.len()))),
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    eprintln!("{}", report.join("\n"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
