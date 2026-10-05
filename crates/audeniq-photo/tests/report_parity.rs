//! color_report must equal `exiftool -j -n -s <color fields>` (minus
//! SourceFile); provenance_fields must contain everything
//! `exiftool -j -n -G1 -s <provenance fields>` reports.
use audeniq_photo::{color_report, metadata, provenance_fields};
use serde_json::Value;
use std::process::Command;

const COLOR: &[&str] = &["ColorSpace", "ColorType", "PhotometricInterpretation", "SamplesPerPixel", "BitsPerSample", "BitDepth", "ColorComponents", "ProfileDescription", "ProfileID", "Orientation"];
const PROV: &[&str] = &["Software", "CreatorTool", "DigitalSourceType", "Description", "Comment", "Encoder", "UserComment", "Parameters", "GenerationParameters", "Prompt", "Workflow"];

fn exiftool(path: &std::path::Path, args: &[&str], names: &[&str]) -> Option<serde_json::Map<String, Value>> {
    let mut c = Command::new("exiftool");
    c.args(args);
    for n in names {
        c.arg(format!("-{n}"));
    }
    let o = c.arg("--").arg(path).output().ok()?;
    let v: Value = serde_json::from_slice(&o.stdout).ok()?;
    let mut m = v.as_array()?.first()?.as_object()?.clone();
    m.remove("SourceFile");
    Some(m)
}

#[test]
fn reports_match_exiftool() {
    if Command::new("exiftool").arg("-ver").output().is_err() {
        eprintln!("exiftool unavailable; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("audeniq-photo-report-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let py = r#"
import sys, os
from PIL import Image, PngImagePlugin
d=sys.argv[1]
im=Image.new("RGB",(20,10),"blue")
im.save(f"{d}/a.png"); im.convert("RGBA").save(f"{d}/b.png"); im.convert("P").save(f"{d}/c.png")
im.convert("L").save(f"{d}/d.jpg"); im.convert("CMYK").save(f"{d}/e.jpg")
ex=Image.Exif(); ex[0x0112]=6; ex[0x0131]="Midjourney"; im.save(f"{d}/f.jpg", exif=ex.tobytes())
p="/usr/share/color/icc/compatibleWithAdobeRGB1998.icc"
if os.path.exists(p): im.save(f"{d}/g.jpg", icc_profile=open(p,"rb").read())
i=PngImagePlugin.PngInfo(); i.add_text("prompt",'{"1":{"class_type":"KSampler","inputs":{}}}'); i.add_text("workflow","x")
im.save(f"{d}/h.png", pnginfo=i)
"#;
    if !Command::new("python3").args(["-c", py]).arg(&dir).status().map(|s| s.success()).unwrap_or(false) {
        eprintln!("Pillow unavailable; skipping");
        return;
    }
    let mut failures = Vec::new();
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = e.path();
        let meta = metadata(&std::fs::read(&path).unwrap()).unwrap();
        let ours = color_report(&meta);
        let theirs = exiftool(&path, &["-j", "-n", "-s"], COLOR).unwrap();
        if Value::Object(ours.clone()) != Value::Object(theirs.clone()) {
            failures.push(format!("{}: color ours {ours:?} exiftool {theirs:?}", path.display()));
        }
        let ours = provenance_fields(&meta);
        let theirs = exiftool(&path, &["-j", "-n", "-G1", "-s"], PROV).unwrap();
        for (k, v) in &theirs {
            if ours.get(k) != Some(v) {
                failures.push(format!("{}: provenance {k}={v} ours {:?}", path.display(), ours.get(k)));
            }
        }
        n += 1;
    }
    std::fs::remove_dir_all(&dir).ok();
    assert!(n >= 8);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
