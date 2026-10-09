//! Peak heap and wall time of the backend entry points on given files
//! (development aid): `heap_peak FILE...`.
//!
//! Peak heap is measured with a counting allocator above the input buffer,
//! so it is the operation's own working set, independent of RSS noise.
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

fn measure<T>(label: &str, file: &str, f: impl Fn() -> T) {
    // Warm-up run (thread pools, lazy tables), then the best of five.
    drop(f());
    let (mut ms, mut peak) = (f64::MAX, 0);
    for _ in 0..5 {
        let base = CUR.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t0 = Instant::now();
        let r = f();
        ms = ms.min(t0.elapsed().as_secs_f64() * 1000.0);
        peak = peak.max(PEAK.load(Ordering::Relaxed) - base);
        drop(r);
    }
    println!(
        "{file:<14} {label:<9} {ms:>8.1} ms {:>8.1} MB",
        peak as f64 / 1e6
    );
}

fn main() {
    let deadline = || Deadline::NONE;
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("read input");
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if data.starts_with(b"%PDF") {
            measure("pdf", &name, || {
                audeniq_photo::pdf::sanitize_pdf(&data, &deadline())
            });
            continue;
        }
        let kind = if data.starts_with(b"\x89PNG") {
            Kind::Png
        } else {
            Kind::Jpeg
        };
        measure("qr", &name, || audeniq_photo::qr_count(&data, &deadline()));
        measure("cover", &name, || {
            audeniq_photo::inspect_cover(&data, &deadline())
        });
        measure("sanitize", &name, || {
            audeniq_photo::sanitize(&data, kind, &deadline())
        });
    }
}
