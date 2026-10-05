//! Decoded-QR counts versus zbarimg on generated covers. Skips when
//! qrencode / zbarimg / Pillow are unavailable.
use photo_core::{Deadline, Limits};
use std::process::Command;

const SCRIPT: &str = r#"
import sys, subprocess, random, io
from PIL import Image, ImageDraw
d = sys.argv[1]
random.seed(7)
def qr(text, version=None, level="M", size=6, margin=4):
    args = ["qrencode", "-o", "-", "-s", str(size), "-m", str(margin), "-l", level]
    if version: args += ["-v", str(version)]
    png = subprocess.run(args + [text], capture_output=True, check=True).stdout
    return Image.open(io.BytesIO(png)).convert("RGB")
def background(w, h, kind):
    im = Image.new("RGB", (w, h), (200, 180, 160))
    dr = ImageDraw.Draw(im)
    if kind == "noise":
        px = im.load()
        for y in range(0, h, 3):
            for x in range(0, w, 3):
                v = random.randint(0, 255)
                for yy in range(y, min(y+3, h)):
                    for xx in range(x, min(x+3, w)):
                        px[xx, yy] = (v, (v * 7) % 256, 255 - v)
    elif kind == "shapes":
        for i in range(60):
            x, y = random.randint(0, w), random.randint(0, h)
            r = random.randint(5, 80)
            c = tuple(random.randint(0, 255) for _ in range(3))
            dr.rectangle([x, y, x + r, y + r], fill=c)
    return im
cases = []
def save(name, im, expected):
    im.save(f"{d}/{name}.png")
    cases.append(f"{name} {expected}")
for v in [1, 2, 5, 7, 10, 15, 25, 40]:
    for level in ["L", "H"]:
        save(f"v{v}{level}", qr("https://example.invalid/" + "x" * (v * 3), v, level, size=4 if v > 20 else 6), 1)
bg = background(900, 900, "shapes")
bg.paste(qr("promo code 1234", 3), (50, 60))
bg.paste(qr("second code", 4, "Q", 5), (500, 450))
save("two_on_shapes", bg, 2)
bg = background(800, 800, "noise")
bg.paste(qr("on noise", 2, "H", 8, 4), (200, 200))
save("noise", bg, 1)
for angle in [15, 45, 90, 180]:
    im = background(700, 700, "plain")
    code = qr("rotated", 4, "M", 6).rotate(angle, expand=True, fillcolor=(255, 255, 255))
    im.paste(code, (100, 100))
    save(f"rot{angle}", im, 1)
small = qr("tiny", 1, "L", 2, 2)
save("tiny", small, 1)
im = qr("scaled", 6, "M", 3).resize((420, 420), Image.BILINEAR)
save("scaled", im, 1)
for kind in ["plain", "noise", "shapes"]:
    save(f"empty_{kind}", background(640, 480, kind), 0)
big = background(3000, 3000, "shapes")
big.paste(qr("cover art", 5, "M", 12), (2000, 2200))
save("cover3000", big, 1)
print("\n".join(cases))
"#;

#[test]
fn counts_match_zbarimg() {
    let dir = std::env::temp_dir().join(format!("photo-qr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Ok(out) = Command::new("python3").args(["-c", SCRIPT]).arg(&dir).output() else {
        eprintln!("python3 unavailable; skipping");
        return;
    };
    if !out.status.success() {
        eprintln!("fixtures unavailable; skipping: {}", String::from_utf8_lossy(&out.stderr));
        return;
    }
    let zbar_ok = Command::new("zbarimg").arg("--version").output().is_ok();
    let mut report = Vec::new();
    let mut mismatches = Vec::new();
    for line in String::from_utf8(out.stdout).unwrap().lines() {
        let (name, expected) = line.split_once(' ').unwrap();
        let expected: usize = expected.parse().unwrap();
        let path = dir.join(format!("{name}.png"));
        let data = std::fs::read(&path).unwrap();
        let (_, img) = photo_png::decode(&data, &Limits::default(), &Deadline::NONE).unwrap();
        let gray = photo_qr::to_gray(&img.data, img.format.channels());
        let t = std::time::Instant::now();
        let s = photo_qr::scan(&gray, img.width as usize, img.height as usize, &Deadline::NONE).unwrap();
        let ours_ms = t.elapsed().as_secs_f64() * 1000.0;
        let zbar = if zbar_ok {
            let o = Command::new("zbarimg").args(["--quiet", "--raw", "-Sdisable", "-Sqrcode.enable"]).arg(&path).output().unwrap();
            // Each decoded symbol prints one line (payloads here are single-line).
            Some(String::from_utf8_lossy(&o.stdout).lines().count())
        } else {
            None
        };
        report.push(format!("{name}: expected {expected} ours {} (cand {}) zbar {:?} [{ours_ms:.1} ms]", s.decoded, s.candidates, zbar));
        if s.decoded != expected {
            mismatches.push(name.to_string());
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    eprintln!("{}", report.join("\n"));
    assert!(mismatches.is_empty(), "wrong counts: {mismatches:?}");
}
