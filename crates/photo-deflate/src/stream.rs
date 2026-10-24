//! Bounded inflate and streaming deflate, calling the engine directly.

use crate::engine::{
    deflate::{self, DeflateConfig, DeflateStream, Strategy},
    inflate::{self, InflateConfig, InflateStream},
    DeflateFlush, InflateFlush, ReturnCode,
};
use photo_core::{Error, Result};
use std::cell::RefCell;

/// Compression level, 0 (stored) to 9 (best). Same scale as zlib.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level(u8);

impl Level {
    pub const STORE: Level = Level(0);
    pub const FAST: Level = Level(1);
    pub const DEFAULT: Level = Level(6);
    pub const BEST: Level = Level(9);
    pub fn new(level: u8) -> Self {
        Level(level.min(9))
    }
    pub fn get(self) -> u8 {
        self.0
    }
}

impl Default for Level {
    fn default() -> Self {
        Level::DEFAULT
    }
}

/// Owned inflate state (freed through the engine on drop).
struct InflateState(InflateStream<'static>);

impl Drop for InflateState {
    fn drop(&mut self) {
        let _ = inflate::end(&mut self.0);
    }
}

fn inflate_config(zlib: bool) -> InflateConfig {
    InflateConfig {
        window_bits: if zlib { 15 } else { -15 },
    }
}

impl InflateState {
    fn new(zlib: bool) -> Self {
        InflateState(InflateStream::new(inflate_config(zlib)))
    }

    /// Run the engine once. Output goes straight into `out`'s spare
    /// capacity, at most `room` bytes. Returns (code, consumed, produced).
    fn step(&mut self, input: &[u8], out: &mut Vec<u8>, room: usize) -> (ReturnCode, usize, usize) {
        out.reserve(room);
        let spare = out.spare_capacity_mut();
        let room = room.min(spare.len()).min(u32::MAX as usize);
        let avail_in = input.len().min(u32::MAX as usize);
        let s = &mut self.0;
        s.next_in = input.as_ptr().cast_mut();
        s.avail_in = avail_in as u32;
        s.next_out = spare.as_mut_ptr().cast();
        s.avail_out = room as u32;
        // SAFETY: the state was initialized by `InflateStream::new`/reset;
        // next_in/avail_in describe a live shared slice and next_out/
        // avail_out the uninitialized tail of `out`, which the engine only
        // writes; both pointers are cleared before returning.
        #[allow(unsafe_code)]
        let code = unsafe { inflate::inflate(s, InflateFlush::NoFlush) };
        let consumed = avail_in - s.avail_in as usize;
        let produced = room - s.avail_out as usize;
        s.next_in = core::ptr::null_mut();
        s.next_out = core::ptr::null_mut();
        s.avail_in = 0;
        s.avail_out = 0;
        // SAFETY: the engine initialized exactly `produced` bytes at the
        // start of the spare capacity.
        #[allow(unsafe_code)]
        unsafe {
            out.set_len(out.len() + produced);
        }
        (code, consumed, produced)
    }
}

thread_local! {
    /// One reusable inflate state per thread and header mode: PNG text,
    /// iCCP and signature streams no longer allocate a window each.
    static INFLATE_POOL: RefCell<[Option<InflateState>; 2]> = const { RefCell::new([None, None]) };
    static DEFLATE_POOL: RefCell<[Option<DeflateState>; 2]> = const { RefCell::new([None, None]) };
}

fn take_inflate(zlib: bool) -> InflateState {
    let pooled = INFLATE_POOL.with(|p| p.borrow_mut()[usize::from(zlib)].take());
    match pooled {
        Some(mut s) => {
            let _ = inflate::reset_with_config(&mut s.0, inflate_config(zlib));
            s
        }
        None => InflateState::new(zlib),
    }
}

fn give_inflate(zlib: bool, s: InflateState) {
    INFLATE_POOL.with(|p| p.borrow_mut()[usize::from(zlib)] = Some(s));
}

fn engine_error(code: ReturnCode) -> Error {
    match code {
        ReturnCode::NeedDict => Error::Unsupported("zlib preset dictionary"),
        ReturnCode::MemError => Error::Limit("inflate memory"),
        _ => Error::Invalid("corrupt deflate stream"),
    }
}

/// Result of a zlib decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inflated {
    /// Bytes of input consumed, including header and checksum.
    pub consumed: usize,
    /// True when the stream ended (checksum verified); false when decoding
    /// stopped at the output limit in truncating mode.
    pub complete: bool,
}

fn run(
    zlib: bool,
    input: &[&[u8]],
    out: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<Inflated> {
    let mut st = take_inflate(zlib);
    let r = run_with(&mut st, input, out, limit, truncate);
    // Only a stream that finished cleanly goes back to the pool; a failed
    // one is dropped (its state may be mid-error).
    if r.is_ok() {
        give_inflate(zlib, st);
    }
    r
}

/// Input split over several slices (PNG IDAT chunks), read in order.
struct Parts<'a> {
    parts: &'a [&'a [u8]],
    /// Position in `parts[0]`.
    pos: usize,
    consumed: usize,
}

impl<'a> Parts<'a> {
    /// The unread rest of the current slice (empty at the end).
    fn current(&mut self) -> &'a [u8] {
        while let Some((first, rest)) = self.parts.split_first() {
            if self.pos < first.len() {
                return &first[self.pos..];
            }
            self.parts = rest;
            self.pos = 0;
        }
        &[]
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
        self.consumed += n;
    }
}

fn run_with(
    st: &mut InflateState,
    input: &[&[u8]],
    out: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<Inflated> {
    let mut input = Parts {
        parts: input,
        pos: 0,
        consumed: 0,
    };
    loop {
        if out.len() >= limit {
            // Is there more output? Probe with a one-byte budget.
            let mut probe = Vec::with_capacity(1);
            let (code, used, produced) = st.step(input.current(), &mut probe, 1);
            input.advance(used);
            if produced > 0 {
                return if truncate {
                    Ok(Inflated {
                        consumed: input.consumed,
                        complete: false,
                    })
                } else {
                    Err(Error::Limit("decompressed size"))
                };
            }
            match code {
                ReturnCode::StreamEnd => {
                    return Ok(Inflated {
                        consumed: input.consumed,
                        complete: true,
                    })
                }
                ReturnCode::Ok | ReturnCode::BufError if used > 0 => continue,
                ReturnCode::Ok | ReturnCode::BufError => return Err(Error::Truncated),
                other => return Err(engine_error(other)),
            }
        }
        // Grow geometrically: one call fills a pre-sized output (PNG rows)
        // and unknown sizes do not over-reserve.
        let room = (limit - out.len()).min(out.len().max(64 * 1024));
        let (code, used, produced) = st.step(input.current(), out, room);
        input.advance(used);
        match code {
            ReturnCode::StreamEnd => {
                return Ok(Inflated {
                    consumed: input.consumed,
                    complete: true,
                })
            }
            ReturnCode::Ok | ReturnCode::BufError => {
                if produced == 0 && used == 0 {
                    return Err(Error::Truncated);
                }
            }
            other => return Err(engine_error(other)),
        }
    }
}

/// Decode a raw DEFLATE stream, appending to `out`. Returns input bytes consumed.
/// Fails with [`Error::Limit`] if `out` would grow beyond `limit` bytes.
pub fn inflate_raw(input: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize> {
    run(false, &[input], out, limit, false).map(|r| r.consumed)
}

/// Decode a zlib stream into `out`.
///
/// With `truncate == false` exceeding `limit` is an error. With
/// `truncate == true` decoding stops once `limit` bytes are produced and the
/// result reports `complete: false` (the checksum is not verified then).
pub fn inflate_zlib(
    input: &[u8],
    out: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<Inflated> {
    run(true, &[input], out, limit, truncate)
}

/// [`inflate_zlib`] over a stream split into consecutive slices (PNG IDAT
/// chunks), without joining them first. `consumed` counts across slices.
pub fn inflate_zlib_parts(
    input: &[&[u8]],
    out: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<Inflated> {
    run(true, input, out, limit, truncate)
}

/// Decode a complete zlib stream that must produce exactly `expected` bytes
/// and be followed by nothing. Used where the exact bytes are evidence.
pub fn inflate_zlib_exact(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let r = inflate_zlib(input, &mut out, expected, false)?;
    if out.len() != expected || r.consumed != input.len() {
        return Err(Error::Invalid("zlib stream size mismatch"));
    }
    Ok(out)
}

/// Owned deflate state (freed through the engine on drop).
struct DeflateState(DeflateStream<'static>);

impl Drop for DeflateState {
    fn drop(&mut self) {
        let _ = deflate::end(&mut self.0);
    }
}

fn take_deflate(level: Level, zlib: bool, strategy: Strategy) -> DeflateState {
    let pooled = DEFLATE_POOL.with(|p| p.borrow_mut()[usize::from(zlib)].take());
    let level = i32::from(level.0);
    match pooled {
        Some(mut s) => {
            let _ = deflate::reset(&mut s.0);
            let _ = deflate::params(&mut s.0, level, strategy);
            s
        }
        None => DeflateState(DeflateStream::new(DeflateConfig {
            window_bits: if zlib { 15 } else { -15 },
            level,
            strategy,
            ..DeflateConfig::default()
        })),
    }
}

impl Tuning {
    fn strategy(self) -> Strategy {
        match self {
            Tuning::Default => Strategy::Default,
            Tuning::Image => Strategy::Image,
        }
    }
}

/// Match-finding policy of a [`Compressor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tuning {
    /// zlib's (zlib-ng's) per-level match finders.
    #[default]
    Default,
    /// Long matches only, accepted by estimated bit cost: for filtered
    /// photographic pixels (smaller and several times faster there; flat
    /// graphics still compress well, noise shared across channels less).
    Image,
}

/// Streaming compressor producing a raw DEFLATE or a zlib stream.
pub struct Compressor {
    st: Option<DeflateState>,
    zlib: bool,
}

const STEP: usize = 64 * 1024;

impl Compressor {
    /// Raw DEFLATE (no header, no checksum).
    pub fn raw(level: Level) -> Self {
        Compressor {
            st: Some(take_deflate(level, false, Strategy::Default)),
            zlib: false,
        }
    }

    /// zlib-wrapped stream (RFC 1950).
    pub fn zlib(level: Level) -> Self {
        Compressor {
            st: Some(take_deflate(level, true, Strategy::Default)),
            zlib: true,
        }
    }

    /// zlib-wrapped stream tuned for PNG-filtered pixel rows: only long
    /// matches are coded (see the engine's `Strategy::Image`). `level`
    /// sets the match search effort (hash chain length).
    pub fn zlib_image(level: Level) -> Self {
        Self::zlib_tuned(level, Tuning::Image)
    }

    /// zlib-wrapped stream with the given [`Tuning`].
    pub fn zlib_tuned(level: Level, tuning: Tuning) -> Self {
        Compressor {
            st: Some(take_deflate(level, true, tuning.strategy())),
            zlib: true,
        }
    }

    /// One piece of a zlib stream compressed as independent pieces (pigz
    /// style, so pieces can be compressed in parallel). The first piece
    /// writes the zlib header; later ones are raw DEFLATE primed with
    /// `dictionary`, the input just before them (only the last 32 KiB
    /// matter). End every piece but the last with
    /// [`Compressor::end_piece`] (byte-aligned, stream left open) and the
    /// last with [`Compressor::finish_piece`]; concatenated, followed by
    /// [`zlib_trailer`] of the whole input, they form one zlib stream.
    pub fn zlib_piece(level: Level, tuning: Tuning, first: bool, dictionary: &[u8]) -> Self {
        let mut st = take_deflate(level, first, tuning.strategy());
        if !first && !dictionary.is_empty() {
            let window = &dictionary[dictionary.len().saturating_sub(32 * 1024)..];
            let code = deflate::set_dictionary(&mut st.0, window);
            assert!(
                code == ReturnCode::Ok,
                "fresh deflate state takes a dictionary"
            );
        }
        Compressor {
            st: Some(st),
            zlib: first,
        }
    }

    /// End a piece that is not the last: flush to a byte boundary without
    /// ending the stream.
    pub fn end_piece(mut self, out: &mut Vec<u8>) {
        self.pump(&[], DeflateFlush::SyncFlush, out);
        self.recycle();
    }

    /// End the last piece (final block; the caller appends the trailer).
    pub fn finish_piece(self, out: &mut Vec<u8>) {
        assert!(!self.zlib, "the last piece is not the first");
        self.finish(out);
    }

    fn recycle(&mut self) {
        let st = self.st.take().expect("compressor is live");
        let zlib = self.zlib;
        DEFLATE_POOL.with(|p| p.borrow_mut()[usize::from(zlib)] = Some(st));
    }

    fn pump(&mut self, mut input: &[u8], flush: DeflateFlush, out: &mut Vec<u8>) {
        let s = &mut self.st.as_mut().expect("compressor is live").0;
        loop {
            out.reserve(STEP);
            let spare = out.spare_capacity_mut();
            let room = spare.len().min(u32::MAX as usize);
            let avail_in = input.len().min(u32::MAX as usize);
            s.next_in = input.as_ptr().cast_mut();
            s.avail_in = avail_in as u32;
            s.next_out = spare.as_mut_ptr().cast();
            s.avail_out = room as u32;
            let code = deflate::deflate(s, flush);
            let used = avail_in - s.avail_in as usize;
            let produced = room - s.avail_out as usize;
            s.next_in = core::ptr::null_mut();
            s.next_out = core::ptr::null_mut();
            s.avail_in = 0;
            s.avail_out = 0;
            // SAFETY: the engine initialized exactly `produced` bytes at the
            // start of the spare capacity (next_out/avail_out above).
            #[allow(unsafe_code)]
            unsafe {
                out.set_len(out.len() + produced);
            }
            input = &input[used..];
            assert!(
                !matches!(code, ReturnCode::StreamError | ReturnCode::DataError),
                "deflate state invalid"
            );
            let done = match flush {
                DeflateFlush::Finish => code == ReturnCode::StreamEnd,
                _ => input.is_empty() && produced < room,
            };
            if done {
                return;
            }
        }
    }

    /// Compress `data`, appending any completed output to `out`.
    pub fn write(&mut self, data: &[u8], out: &mut Vec<u8>) {
        if !data.is_empty() {
            self.pump(data, DeflateFlush::NoFlush, out);
        }
    }

    /// Finish the stream (final block and zlib trailer).
    pub fn finish(mut self, out: &mut Vec<u8>) {
        self.pump(&[], DeflateFlush::Finish, out);
        self.recycle();
    }
}

/// The zlib trailer for input with Adler-32 `adler` (see
/// [`Compressor::zlib_piece`] and [`adler32_combine`]).
pub fn zlib_trailer(adler: u32) -> [u8; 4] {
    adler.to_be_bytes()
}

/// One-shot zlib compression.
pub fn compress_zlib(data: &[u8], level: Level) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    let mut c = Compressor::zlib(level);
    c.write(data, &mut out);
    c.finish(&mut out);
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn roundtrips_all_levels() {
        let text = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(3000);
        let mut mixed = noise(70_000, 7);
        mixed.extend_from_slice(&text);
        mixed.extend(std::iter::repeat_n(0u8, 100_000));
        for level in 0..=9 {
            for data in [&b""[..], b"a", &text, &mixed, &noise(200_000, 3)] {
                let z = compress_zlib(data, Level::new(level));
                let mut out = Vec::new();
                let r = inflate_zlib(&z, &mut out, data.len(), false).unwrap();
                assert!(
                    r.complete && r.consumed == z.len() && out == data,
                    "level {level}"
                );
            }
        }
    }

    #[test]
    fn streaming_in_small_pieces() {
        let data = noise(50_000, 11).repeat(4);
        let mut c = Compressor::raw(Level::BEST);
        let mut z = Vec::new();
        for chunk in data.chunks(777) {
            c.write(chunk, &mut z);
        }
        c.finish(&mut z);
        let mut out = Vec::new();
        assert_eq!(inflate_raw(&z, &mut out, usize::MAX).unwrap(), z.len());
        assert_eq!(out, data);
    }

    #[test]
    fn limits_truncation_and_exactness() {
        let data = vec![7u8; 1 << 20];
        let z = compress_zlib(&data, Level::DEFAULT);
        assert!(z.len() < 3_000);
        let mut out = Vec::new();
        assert_eq!(
            inflate_zlib(&z, &mut out, data.len() - 1, false),
            Err(Error::Limit("decompressed size"))
        );
        let mut out = Vec::new();
        let r = inflate_zlib(&z, &mut out, 10, true).unwrap();
        assert!(!r.complete && out.len() == 10);
        assert_eq!(inflate_zlib_exact(&z, data.len()).unwrap(), data);
        let mut trailing = z.clone();
        trailing.push(0);
        assert!(inflate_zlib_exact(&trailing, data.len()).is_err());
        for cut in [0, 1, 2, z.len() / 2, z.len() - 1] {
            let mut out = Vec::new();
            assert!(
                inflate_zlib(&z[..cut], &mut out, data.len(), false).is_err(),
                "cut {cut}"
            );
        }
    }

    #[test]
    fn split_input_matches_contiguous() {
        let mut data = noise(60_000, 5);
        data.extend(b"abcabcabd".repeat(20_000));
        let z = compress_zlib(&data, Level::DEFAULT);
        let mut want = Vec::new();
        let full = inflate_zlib(&z, &mut want, data.len(), false).unwrap();
        // Splits at every kind of boundary, with empty parts in between.
        for size in [1, 2, 7, 4096, 65_536, z.len()] {
            let mut parts: Vec<&[u8]> = vec![&[]];
            for c in z.chunks(size) {
                parts.extend([c, &[][..]]);
            }
            let mut out = Vec::new();
            let r = inflate_zlib_parts(&parts, &mut out, data.len(), false).unwrap();
            assert!(r == full && out == data, "parts of {size}");
            // Truncating mode stops at the limit across parts too.
            let mut out = Vec::new();
            let r = inflate_zlib_parts(&parts, &mut out, 100_000, true).unwrap();
            assert!(
                !r.complete && out[..] == data[..100_000],
                "truncated {size}"
            );
            // A missing tail is still an error.
            let cut = parts.len() / 2;
            let mut out = Vec::new();
            if z.len() > size {
                assert!(inflate_zlib_parts(&parts[..cut], &mut out, data.len(), false).is_err());
            }
        }
        assert!(inflate_zlib_parts(&[], &mut Vec::new(), 10, false).is_err());
    }
}
