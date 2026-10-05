//! `audeniq-photo` — command-line front end for the library.
//!
//! ```text
//! audeniq-photo probe <file>                 ffprobe-style JSON (format, width, height)
//! audeniq-photo meta <file> [-TAG ...]       ExifTool-style JSON (-j -n -G1), all or selected tags
//! audeniq-photo color <file>                 color properties (artwork policy input)
//! audeniq-photo provenance <file>            provenance fields (AI metadata signals input)
//! audeniq-photo qr <file>                    decoded QR count
//! audeniq-photo cover <file>                 all of the above in one pass
//! audeniq-photo sanitize <src> <dst> <mime>  drop-in for sanitize-upload.py
//! audeniq-photo convert <src> <dst.png|dst.jpg> [--quality N]
//! ```
#![forbid(unsafe_code)]

use audeniq_photo::{Deadline, Kind};
use serde_json::{Value, json};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

fn usage() -> ExitCode {
    eprintln!(
        "usage: audeniq-photo <probe|meta|color|provenance|qr|cover> <file>\n       audeniq-photo sanitize <src> <dst> <mime>\n       audeniq-photo convert <src> <dst.png|dst.jpg> [--quality N]\n       audeniq-photo --version"
    );
    ExitCode::from(2)
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).expect("json"));
}

fn deadline() -> Deadline {
    Deadline::after(Duration::from_secs(120))
}

fn meta_json(path: &str, data: &[u8], names: &[String]) -> Result<Value, String> {
    let m = audeniq_photo::metadata(data).map_err(|e| e.to_string())?;
    let mut obj = serde_json::Map::new();
    obj.insert("SourceFile".into(), json!(path));
    for t in &m.tags {
        let base = t.name.split('-').next().unwrap_or(&t.name);
        if !names.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(base)) {
            continue;
        }
        let key = format!("{}:{}", t.group, t.name);
        let v = match &t.value {
            photo_meta::Value::Int(i) => json!(i),
            photo_meta::Value::Real(r) => json!(r),
            photo_meta::Value::Text(s) => json!(s),
            photo_meta::Value::List(l) => json!(l),
        };
        obj.entry(key).or_insert(v);
    }
    if !m.warnings.is_empty() {
        obj.insert("Warning".into(), json!(m.warnings));
    }
    Ok(Value::Array(vec![Value::Object(obj)]))
}

fn sanitize_pdf(src: &str, dst: &str) -> Result<(), String> {
    let info = Command::new("pdfinfo")
        .arg(src)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !info.status.success() {
        return Err("pdfinfo failed".into());
    }
    let pages = audeniq_photo::pdf::pages_from_pdfinfo(&info.stdout).map_err(|e| e.to_string())?;
    let dir = std::path::Path::new(dst)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let prefix = dir.join("raster");
    let ok = Command::new("pdftoppm")
        .args(audeniq_photo::pdf::pdftoppm_args(pages))
        .arg(src)
        .arg(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !ok {
        return Err("pdftoppm failed".into());
    }
    let mut rasters: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("raster-") && n.ends_with(".jpg"))
        })
        .collect();
    rasters.sort();
    if rasters.len() != pages as usize {
        return Err("incomplete document rasterization".into());
    }
    let bytes = rasters
        .iter()
        .map(std::fs::read)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let out = audeniq_photo::pdf::image_only_pdf(&bytes, &deadline()).map_err(|e| e.to_string())?;
    std::fs::write(dst, out).map_err(|e| e.to_string())
}

fn run(args: &[String]) -> Result<(), String> {
    let cmd = args.first().map(String::as_str).ok_or("missing command")?;
    let file = || {
        args.get(1)
            .map(String::as_str)
            .ok_or_else(|| "missing file".to_string())
    };
    match cmd {
        "probe" => {
            let path = file()?;
            let p = audeniq_photo::probe(&read(path)?).map_err(|e| e.to_string())?;
            print(&json!({
                "streams": [{"codec_type": "video", "width": p.width, "height": p.height}],
                "format": {"filename": path, "format_name": p.format_name()},
            }));
        }
        "meta" => {
            let path = file()?;
            let names: Vec<String> = args[2..]
                .iter()
                .filter_map(|a| a.strip_prefix('-').map(str::to_owned))
                .collect();
            print(&meta_json(path, &read(path)?, &names)?);
        }
        "color" => {
            let m = audeniq_photo::metadata(&read(file()?)?).map_err(|e| e.to_string())?;
            print(
                &json!({"inspection_status": "COMPLETED", "properties": audeniq_photo::color_report(&m)}),
            );
        }
        "provenance" => {
            let m = audeniq_photo::metadata(&read(file()?)?).map_err(|e| e.to_string())?;
            print(&Value::Object(audeniq_photo::provenance_fields(&m)));
        }
        "qr" => {
            let n =
                audeniq_photo::qr_count(&read(file()?)?, &deadline()).map_err(|e| e.to_string())?;
            print(&json!({"qr_count": n}));
        }
        "cover" => {
            let r = audeniq_photo::inspect_cover(&read(file()?)?, &deadline())
                .map_err(|e| e.to_string())?;
            print(&json!({
                "format_name": r.probe.format_name(),
                "width": r.probe.width,
                "height": r.probe.height,
                "color": r.color,
                "provenance": r.provenance,
                "qr_count": r.qr.ok(),
            }));
        }
        "sanitize" => {
            let (src, dst, mime) = match &args[1..] {
                [s, d, m] => (s.as_str(), d.as_str(), m.as_str()),
                _ => return Err("usage: sanitize <src> <dst> <mime>".into()),
            };
            if mime == "application/pdf" {
                return sanitize_pdf(src, dst);
            }
            let kind = Kind::from_mime(mime).ok_or("unsupported type")?;
            let out = audeniq_photo::sanitize(&read(src)?, kind, &deadline())
                .map_err(|e| e.to_string())?;
            std::fs::write(dst, out).map_err(|e| e.to_string())?;
        }
        "convert" => {
            let (src, dst) = (file()?, args.get(2).ok_or("missing destination")?);
            let quality = args
                .iter()
                .position(|a| a == "--quality")
                .and_then(|i| args.get(i + 1))
                .and_then(|q| q.parse().ok())
                .unwrap_or(90u8);
            let img = audeniq_photo::sanitize::pixels(&read(src)?, &deadline())
                .map_err(|e| e.to_string())?;
            let out = if dst.ends_with(".png") {
                photo_png_encode(&img)?
            } else {
                audeniq_photo::sanitize::encode_jpeg(&img, quality).map_err(|e| e.to_string())?
            };
            std::fs::write(dst, out).map_err(|e| e.to_string())?;
        }
        _ => return Err(format!("unknown command {cmd}")),
    }
    Ok(())
}

fn photo_png_encode(img: &audeniq_photo::Image) -> Result<Vec<u8>, String> {
    audeniq_photo::sanitize::encode_png(img).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return usage();
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("audeniq-photo {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let sanitize = args[0] == "sanitize";
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Like the Python sanitizer, never echo parser details that may
            // contain private document text.
            if sanitize {
                eprintln!("upload sanitization failed");
            } else {
                eprintln!("error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}
