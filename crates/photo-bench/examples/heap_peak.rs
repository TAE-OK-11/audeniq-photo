//! Peak heap and wall time of the backend entry points on given files
//! (development aid): `heap_peak FILE...`.
//!
//! Peak heap is measured with a counting allocator above the input buffer,
//! so it is the operation's own working set; the resident-set rise is shown
//! next to it (memory reserved but never touched is not resident).
use audeniq_photo::{Deadline, Kind};
use mimalloc::MiMalloc as System;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to mimalloc (the binaries' allocator) unchanged and only
// counts sizes.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let n = CUR.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(n, Ordering::Relaxed);
        // SAFETY: same layout contract as the caller's.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CUR.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: `p` came from `alloc` with `l`.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // mimalloc keeps the block when shrinking to at least half its size
        // (the tail stays allocated); otherwise it allocates, copies and
        // frees, so count the moment both blocks exist.
        if new > l.size() || new < l.size() / 2 {
            let n = CUR.fetch_add(new, Ordering::Relaxed) + new;
            PEAK.fetch_max(n, Ordering::Relaxed);
            CUR.fetch_sub(l.size(), Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// `VmRSS`/`VmHWM` of this process in kB.
fn proc_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// Run operation `op` on `data` once; `false` if it is not applicable.
fn run(op: &str, data: &[u8]) -> bool {
    let d = Deadline::NONE;
    let kind = if data.starts_with(b"\x89PNG") {
        Kind::Png
    } else {
        Kind::Jpeg
    };
    match op {
        "pdf" => drop(std::hint::black_box(audeniq_photo::pdf::sanitize_pdf(
            data, &d,
        ))),
        "qr" => drop(std::hint::black_box(audeniq_photo::qr_count(data, &d))),
        "cover" => drop(std::hint::black_box(audeniq_photo::inspect_cover(data, &d))),
        "sanitize" => drop(std::hint::black_box(audeniq_photo::sanitize(
            data, kind, &d,
        ))),
        "ocr" => drop(std::hint::black_box(audeniq_photo::ocr_tsv(data, &d))),
        _ => return false,
    }
    true
}

/// Best-of-five wall time and peak heap (after a warm-up run).
fn measure(op: &str, data: &[u8]) -> (f64, usize) {
    run(op, data);
    let (mut ms, mut peak) = (f64::MAX, 0);
    for _ in 0..5 {
        let base = CUR.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t0 = Instant::now();
        run(op, data);
        ms = ms.min(t0.elapsed().as_secs_f64() * 1000.0);
        peak = peak.max(PEAK.load(Ordering::Relaxed) - base);
    }
    (ms, peak)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--rss-child") {
        // A fresh process: the resident high-water mark of one cold run
        // above the resident size with the input loaded.
        let data = std::fs::read(&args[3]).expect("read input");
        let rss0 = proc_kb("VmRSS:");
        let heap0 = CUR.load(Ordering::Relaxed);
        PEAK.store(heap0, Ordering::Relaxed);
        run(&args[2], &data);
        println!("{}", proc_kb("VmHWM:").saturating_sub(rss0));
        // Heap kept after the call (lazily loaded models and tables).
        eprintln!(
            "cold peak {:.1} MB, retained {:.1} MB",
            PEAK.load(Ordering::Relaxed).saturating_sub(heap0) as f64 / 1e6,
            CUR.load(Ordering::Relaxed).saturating_sub(heap0) as f64 / 1e6
        );
        return;
    }
    let exe = std::env::current_exe().expect("own path");
    for path in &args[1..] {
        let data = std::fs::read(path).expect("read input");
        let name = std::path::Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let ops: &[&str] = if data.starts_with(b"%PDF") {
            &["pdf"]
        } else {
            &["qr", "cover", "sanitize", "ocr"]
        };
        for op in ops {
            let (ms, peak) = measure(op, &data);
            let rss_kb: u64 = std::process::Command::new(&exe)
                .args(["--rss-child", op, path])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok()?.trim().parse().ok())
                .unwrap_or(0);
            println!(
                "{name:<22} {op:<9} {ms:>8.1} ms {:>7.1} MB heap {:>7.1} MB rss",
                peak as f64 / 1e6,
                rss_kb as f64 / 1e3
            );
        }
    }
}
