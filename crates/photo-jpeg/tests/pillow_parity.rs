//! Decoded pixels must match Pillow (libjpeg-turbo, ISLOW, fancy upsampling)
//! exactly. Skips when python3 + Pillow are unavailable.
use photo_core::{Deadline, Limits, PixelFormat};
use std::process::Command;

const SCRIPT: &str = r#"
import sys, io
from PIL import Image
d = sys.argv[1]
def src(w, h):
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        for x in range(w):
            px[x, y] = ((x * 7 + y * 3) % 256, (x * x + y * 5) % 256, ((x ^ y) * 9) % 256)
    return im
cases = []
for (w, h) in [(1, 1), (7, 5), (33, 17), (64, 64), (101, 77)]:
    im = src(w, h)
    for sub in [0, 1, 2]:
        for prog in [False, True]:
            for opt in [False, True]:
                name = f"rgb_{w}x{h}_s{sub}_p{int(prog)}_o{int(opt)}"
                im.save(f"{d}/{name}.jpg", quality=87, subsampling=sub, progressive=prog, optimize=opt)
                cases.append(name)
    im.convert("L").save(f"{d}/gray_{w}x{h}.jpg", quality=80)
    cases.append(f"gray_{w}x{h}")
    im.convert("CMYK").save(f"{d}/cmyk_{w}x{h}.jpg", quality=90)
    cases.append(f"cmyk_{w}x{h}")
    im.save(f"{d}/q100_{w}x{h}.jpg", quality=100, subsampling=0)
    cases.append(f"q100_{w}x{h}")
    try:
        im.save(f"{d}/rst_{w}x{h}.jpg", quality=75, restart_marker_blocks=3)
        cases.append(f"rst_{w}x{h}")
    except TypeError:
        pass
for name in cases:
    with Image.open(f"{d}/{name}.jpg") as im:
        im.load()
        open(f"{d}/{name}.raw", "wb").write(im.tobytes())
        open(f"{d}/{name}.mode", "w").write(im.mode)
print("\n".join(cases))
"#;

fn ffmpeg_cases(dir: &std::path::Path) -> Vec<String> {
    // 4:4:0 and 4:1:1 via ffmpeg's mjpeg encoder (decoded by Pillow).
    let mut out = Vec::new();
    for (name, pix) in [
        ("ff440", "yuvj440p"),
        ("ff411", "yuvj411p"),
        ("ff422", "yuvj422p"),
    ] {
        let path = dir.join(format!("{name}.jpg"));
        let ok = Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=90x58",
                "-frames:v",
                "1",
                "-pix_fmt",
                pix,
                "-q:v",
                "3",
            ])
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            out.push(name.to_string());
        }
    }
    let script = r#"
import sys
from PIL import Image
d=sys.argv[1]
for n in sys.argv[2:]:
    with Image.open(f"{d}/{n}.jpg") as im:
        im.load(); open(f"{d}/{n}.raw","wb").write(im.tobytes()); open(f"{d}/{n}.mode","w").write(im.mode)
"#;
    let mut cmd = Command::new("python3");
    cmd.args(["-c", script]).arg(dir).args(&out);
    if !cmd.status().map(|s| s.success()).unwrap_or(false) {
        return Vec::new();
    }
    out
}

#[test]
fn matches_pillow_bit_for_bit() {
    let dir = std::env::temp_dir().join(format!("photo-jpeg-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Ok(out) = Command::new("python3")
        .args(["-c", SCRIPT])
        .arg(&dir)
        .output()
    else {
        eprintln!("python3 unavailable; skipping");
        return;
    };
    if !out.status.success() {
        eprintln!(
            "Pillow unavailable; skipping: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let mut cases: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    cases.extend(ffmpeg_cases(&dir));
    let mut failures = Vec::new();
    for name in &cases {
        let jpg = std::fs::read(dir.join(format!("{name}.jpg"))).unwrap();
        let want = std::fs::read(dir.join(format!("{name}.raw"))).unwrap();
        let mode = std::fs::read_to_string(dir.join(format!("{name}.mode"))).unwrap();
        let (_, img) = match photo_jpeg::decode(&jpg, &Limits::default(), &Deadline::NONE) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{name}: decode error {e}"));
                continue;
            }
        };
        let expect_fmt = match mode.as_str() {
            "L" => PixelFormat::Gray8,
            "RGB" => PixelFormat::Rgb8,
            "CMYK" => PixelFormat::Cmyk8,
            m => panic!("mode {m}"),
        };
        assert_eq!(img.format, expect_fmt, "{name}");
        if img.data != want {
            let diffs = img.data.iter().zip(&want).filter(|(a, b)| a != b).count();
            let maxd = img
                .data
                .iter()
                .zip(&want)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            failures.push(format!("{name}: {diffs} samples differ (max {maxd})"));
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    eprintln!("compared {} JPEG cases against Pillow", cases.len());
    assert!(cases.len() > 50);
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn luma_matches_libjpeg_grayscale_output() {
    let dir = std::env::temp_dir().join(format!("photo-jpeg-luma-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = r#"
import sys
from PIL import Image
d=sys.argv[1]
im=Image.new("RGB",(77,45))
px=im.load()
for y in range(45):
    for x in range(77):
        px[x,y]=((x*9)%256,(y*13)%256,((x+y)*7)%256)
for sub,prog in [(0,False),(2,False),(2,True),(1,True)]:
    n=f"l{sub}{int(prog)}"
    im.save(f"{d}/{n}.jpg",quality=85,subsampling=sub,progressive=prog)
    g=Image.open(f"{d}/{n}.jpg"); g.draft("L",g.size); g=g.convert("L") if g.mode!="L" else g
    open(f"{d}/{n}.raw","wb").write(g.tobytes())
    print(n)
"#;
    let Ok(out) = Command::new("python3")
        .args(["-c", script])
        .arg(&dir)
        .output()
    else {
        return;
    };
    if !out.status.success() {
        eprintln!("Pillow unavailable; skipping");
        return;
    }
    for n in String::from_utf8(out.stdout).unwrap().lines() {
        let jpg = std::fs::read(dir.join(format!("{n}.jpg"))).unwrap();
        let want = std::fs::read(dir.join(format!("{n}.raw"))).unwrap();
        let (_, img) = photo_jpeg::decode_luma(&jpg, &Limits::default(), &Deadline::NONE).unwrap();
        assert_eq!(img.format, PixelFormat::Gray8);
        assert_eq!(img.data, want, "{n}");
    }
    std::fs::remove_dir_all(&dir).ok();
}
