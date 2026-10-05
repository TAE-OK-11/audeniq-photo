//! `audeniq-photo-bench` — compare the in-process Rust implementation with
//! the external tools the backend used (ffprobe, exiftool, zbarimg and the
//! Python/Pillow sanitizer): wall time, CPU time, peak RSS and throughput.
//!
//! ```text
//! audeniq-photo-bench [--iterations N] [--threads T] [--reference-sanitizer PATH]
//!                     [--json OUT] [--markdown OUT] [FILE ...]
//! ```
//! Without files, a synthetic cover set is generated.
// Only `wait4` (child rusage) needs FFI; everything else is safe Rust.
#![deny(unsafe_code)]

use audeniq_photo::{Deadline, Image, Kind, PixelFormat};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default)]
struct Sample {
    wall: Duration,
    cpu: Duration,
    rss_kb: u64,
}

fn read_proc(path: &str, key: &str) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find(|l| l.starts_with(key))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn thread_cpu() -> Duration {
    let s = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
    Duration::from_nanos(
        s.split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    )
}

/// Measure an in-process operation: wall, thread CPU, peak RSS (VmHWM after
/// resetting it through /proc/self/clear_refs).
fn measure_inproc(mut f: impl FnMut() -> bool) -> Option<Sample> {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let cpu0 = thread_cpu();
    let t0 = Instant::now();
    let ok = f();
    let wall = t0.elapsed();
    let cpu = thread_cpu().saturating_sub(cpu0);
    let rss_kb = read_proc("/proc/self/status", "VmHWM:").unwrap_or(0);
    ok.then_some(Sample { wall, cpu, rss_kb })
}

#[allow(unsafe_code)]
fn wait_rusage(pid: u32) -> Option<(i32, libc::rusage)> {
    let mut status = 0;
    // SAFETY: `rusage` is plain data; wait4 fills it for our own child pid.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid pointers to locals; pid is a child we spawned.
    let r = unsafe { libc::wait4(pid as i32, &mut status, 0, &mut ru) };
    (r == pid as i32).then_some((status, ru))
}

fn tv(t: libc::timeval) -> Duration {
    Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
}

/// Measure external commands run one after another; CPU and RSS from rusage.
fn measure_cmds(cmds: &mut [Command]) -> Option<Sample> {
    let mut total = Sample::default();
    let t0 = Instant::now();
    for c in cmds.iter_mut() {
        let child = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let (status, ru) = wait_rusage(child.id())?;
        std::mem::forget(child);
        if !(libc::WIFEXITED(status) && matches!(libc::WEXITSTATUS(status), 0 | 4)) {
            return None;
        }
        total.cpu += tv(ru.ru_utime) + tv(ru.ru_stime);
        total.rss_kb = total.rss_kb.max(ru.ru_maxrss as u64);
    }
    total.wall = t0.elapsed();
    Some(total)
}

fn median(mut v: Vec<Sample>) -> Option<Sample> {
    if v.is_empty() {
        return None;
    }
    v.sort_by_key(|s| s.wall);
    let mid = v[v.len() / 2];
    let cpu = {
        let mut c: Vec<Duration> = v.iter().map(|s| s.cpu).collect();
        c.sort();
        c[c.len() / 2]
    };
    Some(Sample {
        wall: mid.wall,
        cpu,
        rss_kb: v.iter().map(|s| s.rss_kb).max().unwrap_or(0),
    })
}

fn which(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Deterministic photo-like content: gradients, soft noise and shapes.
fn synthetic(w: u32, h: u32, seed: u32) -> Image {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    let mut x32 = seed.wrapping_mul(2_654_435_761) | 1;
    for y in 0..h {
        for x in 0..w {
            x32 ^= x32 << 13;
            x32 ^= x32 >> 17;
            x32 ^= x32 << 5;
            let n = (x32 & 15) as i32 - 8;
            let cx = x as i32 - (w / 2) as i32;
            let cy = y as i32 - (h / 3) as i32;
            let ring = if (cx * cx + cy * cy) % 40_000 < 9_000 {
                40
            } else {
                0
            };
            let r = ((x * 255 / w) as i32 + n + ring).clamp(0, 255) as u8;
            let g = ((y * 255 / h) as i32 + n - ring / 2).clamp(0, 255) as u8;
            let b = ((((x / 37) ^ (y / 53)) & 0x3F) as i32 * 3 + 60 + n).clamp(0, 255) as u8;
            data.extend_from_slice(&[r, g, b]);
        }
    }
    Image {
        width: w,
        height: h,
        format: PixelFormat::Rgb8,
        data,
    }
}

fn fixtures(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    std::fs::create_dir_all(dir).expect("fixture dir");
    let mut out = Vec::new();
    let cover = synthetic(3000, 3000, 1);
    let p = dir.join("cover_3000.jpg");
    std::fs::write(
        &p,
        photo_jpeg::encode(&cover, 92, photo_jpeg::Subsampling::S420).unwrap(),
    )
    .unwrap();
    out.push((p, "image/jpeg"));
    let p = dir.join("cover_3000.png");
    std::fs::write(
        &p,
        photo_png::encode(&cover, photo_deflate::Level::DEFAULT).unwrap(),
    )
    .unwrap();
    out.push((p, "image/png"));
    let small = synthetic(1400, 1400, 2);
    let p = dir.join("cover_1400.jpg");
    std::fs::write(
        &p,
        photo_jpeg::encode(&small, 88, photo_jpeg::Subsampling::S420).unwrap(),
    )
    .unwrap();
    out.push((p, "image/jpeg"));
    let icc = Path::new("/usr/share/color/icc/compatibleWithAdobeRGB1998.icc");
    if let Ok(profile) = std::fs::read(icc) {
        // Insert APP2 ICC_PROFILE after SOI.
        let jpg = photo_jpeg::encode(&small, 88, photo_jpeg::Subsampling::S420).unwrap();
        let mut seg = b"ICC_PROFILE\0\x01\x01".to_vec();
        seg.extend_from_slice(&profile);
        let mut with = jpg[..2].to_vec();
        with.extend_from_slice(&[0xFF, 0xE2]);
        with.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
        with.extend_from_slice(&seg);
        with.extend_from_slice(&jpg[2..]);
        let p = dir.join("cover_1400_adobergb.jpg");
        std::fs::write(&p, with).unwrap();
        out.push((p, "image/jpeg"));
    }
    let mut sig = Image {
        width: 600,
        height: 200,
        format: PixelFormat::Rgb8,
        data: vec![255; 600 * 200 * 3],
    };
    for t in 0..2000u32 {
        let x = 40 + t * 520 / 2000;
        let y = 100 + ((t as f64 / 60.0).sin() * 50.0) as u32;
        for dy in 0..3 {
            for dx in 0..3 {
                let i = (((y + dy) * 600 + x + dx) * 3) as usize;
                sig.data[i..i + 3].copy_from_slice(&[20, 20, 60]);
            }
        }
    }
    let p = dir.join("signature.png");
    std::fs::write(
        &p,
        photo_png::encode(&sig, photo_deflate::Level::DEFAULT).unwrap(),
    )
    .unwrap();
    out.push((p, "application/x-audeniq-signature"));
    // A 3-page scanned document (JPEG pages) and a 2-page text/vector one.
    let scans: Vec<Vec<u8>> = (0..3)
        .map(|i| {
            let page = synthetic(1700, 2200, 10 + i);
            photo_jpeg::encode(&page, 85, photo_jpeg::Subsampling::S420).unwrap()
        })
        .collect();
    let p = dir.join("document_scan_3p.pdf");
    std::fs::write(
        &p,
        audeniq_photo::pdf::image_only_pdf(&scans, &Deadline::NONE).unwrap(),
    )
    .unwrap();
    out.push((p, "application/pdf"));
    let p = dir.join("document_text_2p.pdf");
    std::fs::write(&p, text_pdf(2)).unwrap();
    out.push((p, "application/pdf"));
    out
}

/// Pages of Helvetica text lines and ruled boxes (contract-like documents).
fn text_pdf(pages: usize) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        Vec::new(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let mut kids = Vec::new();
    for page in 0..pages {
        let mut content = String::from("0.2 g 1 w\n");
        for row in 0..45 {
            let y = 770 - row * 16;
            content.push_str(&format!(
                "BT /F1 10 Tf 56 {y} Td (Clause {page}.{row}: The licensor grants the label a non-exclusive right to distribute the recording.) Tj ET\n"
            ));
            if row % 9 == 0 {
                content.push_str(&format!("50 {} 512 14 re S\n", y - 3));
            }
        }
        let n = objects.len() + 1;
        objects.push(
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", n + 1)
                .into_bytes(),
        );
        let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(content.as_bytes());
        stream.extend_from_slice(b"\nendstream");
        objects.push(stream);
        kids.push(format!("{n} 0 R"));
    }
    objects[1] = format!(
        "<< /Type /Pages /Count {pages} /Kids [{}] >>",
        kids.join(" ")
    )
    .into_bytes();
    let mut data = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(data.len());
        data.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        data.extend_from_slice(o);
        data.extend_from_slice(b"\nendobj\n");
    }
    let start = data.len();
    data.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        data.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    data
}

struct Args {
    iterations: usize,
    threads: usize,
    reference: Option<PathBuf>,
    json: Option<PathBuf>,
    markdown: Option<PathBuf>,
    files: Vec<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args {
        iterations: 5,
        threads: 4,
        reference: None,
        json: None,
        markdown: None,
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--iterations" => {
                a.iterations = it.next().and_then(|v| v.parse().ok()).unwrap_or(5).max(1)
            }
            "--threads" => a.threads = it.next().and_then(|v| v.parse().ok()).unwrap_or(4).max(1),
            "--reference-sanitizer" => a.reference = it.next().map(PathBuf::from),
            "--json" => a.json = it.next().map(PathBuf::from),
            "--markdown" => a.markdown = it.next().map(PathBuf::from),
            "-h" | "--help" => {
                eprintln!(
                    "usage: audeniq-photo-bench [--iterations N] [--threads T] [--reference-sanitizer PATH] [--json OUT] [--markdown OUT] [FILE ...]"
                );
                std::process::exit(0);
            }
            f => a.files.push(PathBuf::from(f)),
        }
    }
    a
}

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("pdf") => "application/pdf",
        _ => "image/jpeg",
    }
}

fn main() {
    let args = parse_args();
    let tmp = std::env::temp_dir().join(format!("audeniq-photo-bench-{}", std::process::id()));
    let all: Vec<(PathBuf, &str)> = if args.files.is_empty() {
        fixtures(&tmp.join("in"))
    } else {
        args.files.iter().map(|f| (f.clone(), mime_of(f))).collect()
    };
    // PDFs are compared against Poppler separately below.
    let (pdfs, files): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|(_, m)| *m == "application/pdf");
    std::fs::create_dir_all(tmp.join("out")).unwrap();
    let have = |t: &str| which(t);
    let (ffprobe, exiftool, zbar, python) = (
        have("ffprobe"),
        have("exiftool"),
        have("zbarimg"),
        have("python3"),
    );
    let reference = args.reference.clone().filter(|p| p.exists());
    let deadline = Deadline::after(Duration::from_secs(600));
    let color_args = [
        "-j",
        "-n",
        "-s",
        "-ColorSpace",
        "-ColorType",
        "-PhotometricInterpretation",
        "-SamplesPerPixel",
        "-BitsPerSample",
        "-BitDepth",
        "-ColorComponents",
        "-ProfileDescription",
        "-ProfileID",
        "-Orientation",
        "--",
    ];
    let prov_args = [
        "-j",
        "-n",
        "-G1",
        "-s",
        "-Software",
        "-CreatorTool",
        "-DigitalSourceType",
        "-Description",
        "-Comment",
        "-Encoder",
        "-UserComment",
        "-Parameters",
        "-GenerationParameters",
        "-Prompt",
        "-Workflow",
        "--",
    ];

    let mut rows = Vec::new();
    for (path, mime) in &files {
        let data = std::fs::read(path).expect("read input");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let out_path = tmp.join("out").join(format!("{name}.out"));
        let kind = Kind::from_mime(mime).unwrap_or(Kind::Jpeg);
        let image_ops = *mime != "application/x-audeniq-signature";
        type Op<'a> = (
            &'static str,
            Box<dyn FnMut() -> bool + 'a>,
            Option<Box<dyn FnMut() -> Vec<Command> + 'a>>,
        );
        let p = path.clone();
        let mut ops: Vec<Op> = Vec::new();
        if image_ops {
            let p1 = p.clone();
            ops.push((
                "probe",
                Box::new(|| audeniq_photo::probe(&data).is_ok()),
                ffprobe.then(|| {
                    Box::new(move || {
                        let mut c = Command::new("ffprobe");
                        c.args([
                            "-v",
                            "error",
                            "-protocol_whitelist",
                            "file",
                            "-show_format",
                            "-show_streams",
                            "-of",
                            "json",
                        ])
                        .arg(&p1);
                        vec![c]
                    }) as Box<dyn FnMut() -> Vec<Command>>
                }),
            ));
            let p2 = p.clone();
            ops.push((
                "color",
                Box::new(|| {
                    audeniq_photo::metadata(&data)
                        .map(|m| audeniq_photo::color_report(&m))
                        .is_ok()
                }),
                exiftool.then(|| {
                    Box::new(move || {
                        let mut c = Command::new("exiftool");
                        c.args(color_args).arg(&p2);
                        vec![c]
                    }) as Box<dyn FnMut() -> Vec<Command>>
                }),
            ));
            let p3 = p.clone();
            ops.push((
                "provenance",
                Box::new(|| {
                    audeniq_photo::metadata(&data)
                        .map(|m| audeniq_photo::provenance_fields(&m))
                        .is_ok()
                }),
                exiftool.then(|| {
                    Box::new(move || {
                        let mut c = Command::new("exiftool");
                        c.args(prov_args).arg(&p3);
                        vec![c]
                    }) as Box<dyn FnMut() -> Vec<Command>>
                }),
            ));
            let p4 = p.clone();
            ops.push((
                "qr",
                Box::new(|| audeniq_photo::qr_count(&data, &deadline).is_ok()),
                zbar.then(|| {
                    Box::new(move || {
                        let mut c = Command::new("zbarimg");
                        c.args([
                            "--quiet",
                            "--nodbus",
                            "--xml",
                            "-Sdisable",
                            "-Sqrcode.enable",
                            "--",
                        ])
                        .arg(&p4);
                        vec![c]
                    }) as Box<dyn FnMut() -> Vec<Command>>
                }),
            ));
            let p5 = p.clone();
            ops.push((
                "cover (all of the above)",
                Box::new(|| audeniq_photo::inspect_cover(&data, &deadline).is_ok()),
                (ffprobe && exiftool && zbar).then(|| {
                    Box::new(move || {
                        let mut a = Command::new("ffprobe");
                        a.args([
                            "-v",
                            "error",
                            "-show_format",
                            "-show_streams",
                            "-of",
                            "json",
                        ])
                        .arg(&p5);
                        let mut b = Command::new("exiftool");
                        b.args(color_args).arg(&p5);
                        let mut c = Command::new("exiftool");
                        c.args(prov_args).arg(&p5);
                        let mut d = Command::new("zbarimg");
                        d.args([
                            "--quiet",
                            "--nodbus",
                            "--xml",
                            "-Sdisable",
                            "-Sqrcode.enable",
                            "--",
                        ])
                        .arg(&p5);
                        vec![a, b, c, d]
                    }) as Box<dyn FnMut() -> Vec<Command>>
                }),
            ));
        }
        let (p6, o6) = (p.clone(), out_path.clone());
        let reference6 = reference.clone();
        ops.push((
            "sanitize",
            Box::new(|| audeniq_photo::sanitize(&data, kind, &deadline).is_ok()),
            (python && reference6.is_some()).then(|| {
                Box::new(move || {
                    let mut c = Command::new("python3");
                    c.arg(reference6.as_ref().unwrap())
                        .arg(&p6)
                        .arg(&o6)
                        .arg(mime);
                    vec![c]
                }) as Box<dyn FnMut() -> Vec<Command>>
            }),
        ));

        for (op, mut ours, theirs) in ops {
            let _ = ours(); // warm-up
            let rust = median(
                (0..args.iterations)
                    .filter_map(|_| measure_inproc(&mut ours))
                    .collect(),
            );
            let ext = theirs.and_then(|mut t| {
                let _ = measure_cmds(&mut t());
                median(
                    (0..args.iterations)
                        .filter_map(|_| measure_cmds(&mut t()))
                        .collect(),
                )
            });
            rows.push((name.clone(), op, rust, ext));
        }
    }

    // PDF: native parse+render+rebuild vs the former pipeline.
    let poppler = have("pdfinfo") && have("pdftoppm");
    for (path, _) in &pdfs {
        let data = std::fs::read(path).expect("read input");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let pages = audeniq_photo::pdf::info(&data).map_or(1, |i| i.pages);
        let mut ours = || audeniq_photo::pdf::sanitize_pdf(&data, &deadline).is_ok();
        let _ = ours();
        let rust = median(
            (0..args.iterations)
                .filter_map(|_| measure_inproc(&mut ours))
                .collect(),
        );
        let raster = tmp.join("out").join("raster");
        let theirs = || {
            let mut info = Command::new("pdfinfo");
            info.arg(path);
            let mut ppm = Command::new("pdftoppm");
            ppm.args(audeniq_photo::pdf::pdftoppm_args(pages))
                .arg(path)
                .arg(&raster);
            vec![info, ppm]
        };
        // The former pipeline: Poppler, then the image-only rebuild from
        // its JPEG rasters (timed in-process and added).
        let former = || -> Option<Sample> {
            let mut s = measure_cmds(&mut theirs())?;
            let mut rasters: Vec<PathBuf> = std::fs::read_dir(tmp.join("out"))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("raster-"))
                })
                .collect();
            rasters.sort();
            let bytes: Vec<Vec<u8>> = rasters
                .iter()
                .filter_map(|p| std::fs::read(p).ok())
                .collect();
            for r in &rasters {
                std::fs::remove_file(r).ok();
            }
            let rebuild =
                measure_inproc(|| audeniq_photo::pdf::image_only_pdf(&bytes, &deadline).is_ok())?;
            s.wall += rebuild.wall;
            s.cpu += rebuild.cpu;
            Some(s)
        };
        let ext = poppler
            .then(|| {
                let _ = former();
                median((0..args.iterations).filter_map(|_| former()).collect())
            })
            .flatten();
        rows.push((
            name,
            "pdf sanitize (vs pdfinfo+pdftoppm+rebuild)",
            rust,
            ext,
        ));
    }

    // Output sizes of the sanitized files (ours vs the reference script).
    let mut sizes = String::from(
        "\n| file | sanitized size (Rust) | sanitized size (Python/Pillow) |\n|---|---:|---:|\n",
    );
    for (path, mime) in &files {
        let data = std::fs::read(path).unwrap();
        let ours = audeniq_photo::sanitize(
            &data,
            Kind::from_mime(mime).unwrap_or(Kind::Jpeg),
            &deadline,
        )
        .map(|v| v.len());
        let theirs = reference.as_ref().filter(|_| python).and_then(|script| {
            let out = tmp.join("out").join("size.ref");
            let ok = Command::new("python3")
                .arg(script)
                .arg(path)
                .arg(&out)
                .arg(mime)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()?
                .success();
            ok.then(|| std::fs::metadata(&out).map(|m| m.len() as usize).ok())
                .flatten()
        });
        let kib = |v: usize| format!("{:.0} KiB", v as f64 / 1024.0);
        sizes.push_str(&format!(
            "| {} | {} | {} |\n",
            path.file_name().unwrap().to_string_lossy(),
            ours.map_or("failed".into(), kib),
            theirs.map_or("—".into(), kib)
        ));
    }

    // Throughput: sanitize every file `threads * iterations` times in parallel.
    let jobs: Vec<(Vec<u8>, Kind)> = files
        .iter()
        .map(|(p, m)| {
            (
                std::fs::read(p).unwrap(),
                Kind::from_mime(m).unwrap_or(Kind::Jpeg),
            )
        })
        .collect();
    let total = args.threads * args.iterations;
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..args.threads {
            s.spawn(|| {
                for _ in 0..args.iterations {
                    for (d, k) in &jobs {
                        let _ = audeniq_photo::sanitize(d, *k, &deadline);
                    }
                }
            });
        }
    });
    let rust_tput = (total * jobs.len()) as f64 / t0.elapsed().as_secs_f64();
    let ext_tput = reference.as_ref().filter(|_| python).map(|script| {
        let t0 = Instant::now();
        std::thread::scope(|s| {
            for t in 0..args.threads {
                let files = &files;
                let tmp = &tmp;
                s.spawn(move || {
                    for i in 0..args.iterations {
                        for (j, (p, m)) in files.iter().enumerate() {
                            let _ = Command::new("python3")
                                .arg(script)
                                .arg(p)
                                .arg(tmp.join("out").join(format!("t{t}-{i}-{j}")))
                                .arg(m)
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .status();
                        }
                    }
                });
            }
        });
        (total * files.len()) as f64 / t0.elapsed().as_secs_f64()
    });

    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let fmt_ms = |d: Duration| {
        let v = ms(d);
        if v < 1.0 {
            format!("{:.0} µs", v * 1000.0)
        } else {
            format!("{v:.1}")
        }
    };
    let mut md = String::from(
        "| file | operation | Rust wall ms | Rust CPU ms | Rust peak RSS MB | external wall ms | external CPU ms | external peak RSS MB | speed-up |\n|---|---|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    let mut jrows = Vec::new();
    for (file, op, r, e) in &rows {
        let f = |s: &Option<Sample>, g: &dyn Fn(&Sample) -> String| {
            s.as_ref().map_or("—".to_string(), g)
        };
        let speed = match (r, e) {
            (Some(r), Some(e)) if r.wall.as_nanos() > 0 => {
                format!("{:.1}×", e.wall.as_secs_f64() / r.wall.as_secs_f64())
            }
            _ => "—".into(),
        };
        md.push_str(&format!(
            "| {file} | {op} | {} | {} | {} | {} | {} | {} | {speed} |\n",
            f(r, &|s| fmt_ms(s.wall)),
            f(r, &|s| fmt_ms(s.cpu)),
            f(r, &|s| format!("{:.1}", s.rss_kb as f64 / 1024.0)),
            f(e, &|s| fmt_ms(s.wall)),
            f(e, &|s| fmt_ms(s.cpu)),
            f(e, &|s| format!("{:.1}", s.rss_kb as f64 / 1024.0)),
        ));
        let js = |s: &Option<Sample>| {
            s.map(|s| json!({"wall_ms": ms(s.wall), "cpu_ms": ms(s.cpu), "peak_rss_kb": s.rss_kb}))
        };
        jrows.push(json!({"file": file, "operation": op, "rust": js(r), "external": js(e)}));
    }
    md.push_str(&format!(
        "\nSanitize throughput with {} threads: Rust {:.1} files/s{}\n",
        args.threads,
        rust_tput,
        ext_tput.map_or(String::new(), |t| format!(
            ", Python/Pillow {t:.1} files/s ({:.1}×)",
            rust_tput / t
        ))
    ));
    md.push_str(&sizes);
    md.push_str("\nRust peak RSS is the whole benchmark process (VmHWM, reset before each run); external figures are the child's ru_maxrss. CPU is user+system time.\n");
    print!("{md}");
    if let Some(p) = &args.markdown {
        std::fs::write(p, &md).expect("write markdown");
    }
    if let Some(p) = &args.json {
        let doc = json!({"iterations": args.iterations, "threads": args.threads, "rows": jrows, "throughput": {"rust_files_per_s": rust_tput, "external_files_per_s": ext_tput}});
        std::fs::write(p, serde_json::to_string_pretty(&doc).unwrap()).expect("write json");
    }
    std::fs::remove_dir_all(&tmp).ok();
}
