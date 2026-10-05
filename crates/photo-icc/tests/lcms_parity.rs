//! Compare against Pillow's ImageCms (LittleCMS 2) on every ICC profile
//! found on the system. Skips when python3/Pillow or profiles are missing.
use photo_icc::{Profile, Transform};
use std::process::Command;

fn profiles() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for dir in ["/usr/share/color/icc", "/usr/share/color/icc/ghostscript"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if matches!(
                    p.extension().and_then(|s| s.to_str()),
                    Some("icc" | "ICM" | "icm")
                ) {
                    v.push(p);
                }
            }
        }
    }
    v.sort();
    v
}

fn source(channels: usize) -> Vec<u8> {
    // Every 8-bit level on each axis plus a pseudo-random spread.
    let mut v = Vec::new();
    let mut x = 12345u32;
    for i in 0..4096u32 {
        for c in 0..channels {
            let level = if i < 256 {
                i as u8
            } else {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345 + c as u32);
                (x >> 16) as u8
            };
            v.push(if i < 256 && c != 0 && channels == 4 {
                0
            } else {
                level
            });
        }
    }
    v
}

#[test]
fn close_to_lcms() {
    let dir = std::env::temp_dir().join(format!("photo-icc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut report = Vec::new();
    let mut worst = 0u8;
    let mut checked = 0;
    for path in profiles() {
        let data = std::fs::read(&path).unwrap();
        let Ok(profile) = Profile::parse(&data) else {
            continue;
        };
        let channels = match &profile.color_space {
            b"RGB " => 3,
            b"GRAY" => 1,
            b"CMYK" => 4,
            _ => continue,
        };
        if &profile.class == b"link" || &profile.class == b"abst" {
            continue;
        }
        let src = source(channels);
        let raw = dir.join("src.raw");
        let out = dir.join("out.raw");
        std::fs::write(&raw, &src).unwrap();
        let mode = ["", "L", "", "RGB", "CMYK"][channels];
        let script = format!(
            "from PIL import Image, ImageCms\nim=Image.frombytes('{mode}',(4096,1),open(r'{}','rb').read())\nr=ImageCms.profileToProfile(im, ImageCms.getOpenProfile(r'{}'), ImageCms.createProfile('sRGB'), outputMode='RGB')\nopen(r'{}','wb').write(r.tobytes())",
            raw.display(),
            path.display(),
            out.display()
        );
        let ok = Command::new("python3")
            .args(["-c", &script])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            report.push(format!("{}: lcms refused", path.display()));
            continue;
        }
        let want = std::fs::read(&out).unwrap();
        let t = match Transform::to_srgb(&profile, channels) {
            Ok(t) => t,
            Err(e) => {
                report.push(format!("{}: ours refused: {e}", path.display()));
                continue;
            }
        };
        let mut got = vec![0u8; 4096 * 3];
        t.convert(&src, &mut got);
        let diffs: Vec<u8> = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).collect();
        let max = *diffs.iter().max().unwrap();
        let mean = diffs.iter().map(|&d| f64::from(d)).sum::<f64>() / diffs.len() as f64;
        report.push(format!(
            "{} ({}ch v{:x}): max {max} mean {mean:.3}",
            path.file_name().unwrap().to_string_lossy(),
            channels,
            profile.version >> 24
        ));
        worst = worst.max(max);
        checked += 1;
    }
    std::fs::remove_dir_all(&dir).ok();
    eprintln!("{}", report.join("\n"));
    if checked == 0 {
        eprintln!("no profiles or Pillow; skipping");
        return;
    }
    assert!(worst <= 3, "worst difference {worst}");
}
