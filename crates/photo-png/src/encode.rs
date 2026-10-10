//! Streaming PNG encoder. Emits only IHDR, IDAT and IEND, so nothing from a
//! source file (text, EXIF, ICC, appended data) can survive re-encoding.

use photo_core::{Error, Image, PixelFormat, Result};
use photo_deflate::{Adler32, Compressor, Crc32, Level, Tuning, adler32_combine, zlib_trailer};
use std::io::Write;

const IDAT_CHUNK: usize = 64 * 1024;

fn write_chunk<W: Write>(w: &mut W, kind: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(kind)?;
    w.write_all(data)?;
    let mut c = Crc32::new();
    c.update(kind);
    c.update(data);
    w.write_all(&c.finish().to_be_bytes())
}

fn io(_: std::io::Error) -> Error {
    Error::Invalid("write failed")
}

/// Signature and IHDR.
fn write_header<W: Write>(out: &mut W, width: u32, height: u32, format: PixelFormat) -> Result<()> {
    let color_type = match format {
        PixelFormat::Gray8 => 0,
        PixelFormat::GrayAlpha8 => 4,
        PixelFormat::Rgb8 => 2,
        PixelFormat::Rgba8 => 6,
        PixelFormat::Cmyk8 => return Err(Error::Unsupported("CMYK PNG")),
    };
    if width == 0 || height == 0 || width > 0x7FFF_FFFF || height > 0x7FFF_FFFF {
        return Err(Error::Invalid("PNG dimensions"));
    }
    out.write_all(&crate::SIGNATURE).map_err(io)?;
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = color_type;
    write_chunk(out, b"IHDR", &ihdr).map_err(io)
}

pub struct Encoder<W: Write> {
    out: W,
    z: Option<Compressor>,
    buf: Vec<u8>,
    bpp: usize,
    stride: usize,
    rows_left: u32,
    prev: Vec<u8>,
    filtered: Vec<u8>,
}

impl<W: Write> Encoder<W> {
    pub fn new(out: W, width: u32, height: u32, format: PixelFormat, level: Level) -> Result<Self> {
        Self::with_tuning(out, width, height, format, level, Tuning::Image)
    }

    /// As [`Encoder::new`], with the zlib match-finding policy given.
    pub fn with_tuning(
        mut out: W,
        width: u32,
        height: u32,
        format: PixelFormat,
        level: Level,
        tuning: Tuning,
    ) -> Result<Self> {
        write_header(&mut out, width, height, format)?;
        let bpp = format.channels();
        let stride = width as usize * bpp;
        Ok(Encoder {
            out,
            z: Some(Compressor::zlib_tuned(level, tuning)),
            buf: Vec::with_capacity(IDAT_CHUNK * 2),
            bpp,
            stride,
            rows_left: height,
            // The row above the first is all zeros (PNG's definition), so
            // Up/Paeth there equal None/Sub and lose the tie to them.
            prev: vec![0; stride],
            filtered: vec![0; stride + 1],
        })
    }

    /// Append one row of `width * channels` bytes.
    pub fn write_row(&mut self, row: &[u8]) -> Result<()> {
        if row.len() != self.stride || self.rows_left == 0 {
            return Err(Error::Invalid("row size"));
        }
        self.rows_left -= 1;
        let filter = filter_row(row, &self.prev, self.bpp, &mut self.filtered[1..]);
        self.filtered[0] = filter;
        let z = self.z.as_mut().expect("encoder active");
        z.write(&self.filtered, &mut self.buf);
        self.prev.copy_from_slice(row);
        self.flush_idat(false)
    }

    fn flush_idat(&mut self, all: bool) -> Result<()> {
        while self.buf.len() >= IDAT_CHUNK || (all && !self.buf.is_empty()) {
            let n = self.buf.len().min(IDAT_CHUNK);
            write_chunk(&mut self.out, b"IDAT", &self.buf[..n]).map_err(io)?;
            self.buf.drain(..n);
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        if self.rows_left != 0 {
            return Err(Error::Invalid("missing rows"));
        }
        let z = self.z.take().expect("encoder active");
        z.finish(&mut self.buf);
        self.flush_idat(true)?;
        write_chunk(&mut self.out, b"IEND", &[]).map_err(io)?;
        self.out.flush().map_err(io)?;
        Ok(self.out)
    }
}

/// Encode a whole image into a PNG byte vector.
///
/// Images of two or more [`PIECE`]s of raw data are compressed as
/// independent pieces in parallel (the same filtered rows, so the same
/// pixels; the zlib stream differs from the one-piece encoding by a few
/// bytes per piece). The pieces depend only on the image, never on the
/// number of threads, so the output is the same on every machine.
pub fn encode(img: &Image, level: Level) -> Result<Vec<u8>> {
    let tuning = choose_tuning(img, level);
    let stride = img.stride();
    let rows_per_piece = PIECE.div_ceil(stride.max(1));
    if level.get() > 0 && img.height as usize >= 2 * rows_per_piece {
        return encode_pieces(img, level, tuning, rows_per_piece);
    }
    let mut e = Encoder::with_tuning(
        Vec::with_capacity(img.data.len() / 2),
        img.width,
        img.height,
        img.format,
        level,
        tuning,
    )?;
    for row in img.data.chunks_exact(img.stride()) {
        e.write_row(row)?;
    }
    e.finish()
}

/// Raw bytes per piece of a parallel encode (rounded up to whole rows).
const PIECE: usize = 2 << 20;

/// Filtered rows `rows` of `img` (filter byte first), as [`Encoder`] makes
/// them, handed to `f` a few rows at a time.
fn filtered_rows(img: &Image, rows: std::ops::Range<usize>, mut f: impl FnMut(&[u8])) {
    let stride = img.stride();
    let bpp = img.format.channels();
    let zero = vec![0u8; stride];
    let batch = (IDAT_CHUNK / (stride + 1)).max(1);
    let mut buf = Vec::with_capacity(batch * (stride + 1));
    let mut line = vec![0u8; stride];
    for y in rows {
        let row = &img.data[y * stride..(y + 1) * stride];
        let prev = if y == 0 {
            &zero[..]
        } else {
            &img.data[(y - 1) * stride..y * stride]
        };
        buf.push(filter_row(row, prev, bpp, &mut line));
        buf.extend_from_slice(&line);
        if buf.len() >= batch * (stride + 1) {
            f(&buf);
            buf.clear();
        }
    }
    if !buf.is_empty() {
        f(&buf);
    }
}

/// [`encode`] for large images: compress row pieces on up to
/// `available_parallelism` threads, then write the PNG.
fn encode_pieces(
    img: &Image,
    level: Level,
    tuning: Tuning,
    rows_per_piece: usize,
) -> Result<Vec<u8>> {
    let h = img.height as usize;
    let stride = img.stride();
    let pieces: Vec<std::ops::Range<usize>> = (0..h)
        .step_by(rows_per_piece)
        .map(|y| y..(y + rows_per_piece).min(h))
        .collect();
    // Rows before a piece whose filtered bytes cover the 32 KiB window.
    let dict_rows = (32 * 1024usize).div_ceil(stride + 1);
    let compress = |i: usize| {
        let rows = pieces[i].clone();
        let mut dictionary = Vec::new();
        if i > 0 {
            filtered_rows(img, rows.start.saturating_sub(dict_rows)..rows.start, |b| {
                dictionary.extend_from_slice(b)
            });
        }
        let last = i + 1 == pieces.len();
        let mut z = Compressor::zlib_piece(level, tuning, i == 0, &dictionary);
        let mut out = Vec::with_capacity(rows.len() * stride / 2);
        let mut adler = Adler32::new();
        let mut len = 0u64;
        filtered_rows(img, rows, |b| {
            adler.update(b);
            len += b.len() as u64;
            z.write(b, &mut out);
        });
        if last {
            z.finish_piece(&mut out);
        } else {
            z.end_piece(&mut out);
        }
        // A piece may wait for earlier ones: keep only its bytes.
        out.shrink_to_fit();
        (out, adler.finish(), len)
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(pieces.len());
    let mut out = Vec::new();
    write_header(&mut out, img.width, img.height, img.format)?;
    // Pieces are written as soon as every earlier one is, then freed, so
    // only pieces finished out of order wait (not the whole stream).
    let state = std::sync::Mutex::new(Assembly {
        out: Assembler::new(out),
        waiting: (0..pieces.len()).map(|_| None).collect(),
        next: 0,
    });
    let deliver = |i: usize, piece: (Vec<u8>, u32, u64)| {
        let mut guard = state.lock().expect("no worker panicked");
        let Assembly { out, waiting, next } = &mut *guard;
        waiting[i] = Some(piece);
        if *next == 0
            && let Some(first) = &waiting[0]
        {
            // Size the file from the first piece's ratio (plus a quarter);
            // pieces of other content only grow it.
            let n = waiting.len();
            out.reserve(first.0.len() * n + first.0.len() * n / 4 + IDAT_CHUNK);
        }
        while let Some(p) = waiting.get_mut(*next).and_then(Option::take) {
            out.piece(p);
            *next += 1;
        }
    };
    if threads <= 1 {
        for i in 0..pieces.len() {
            deliver(i, compress(i));
        }
    } else {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let work = || {
            loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= pieces.len() {
                    break;
                }
                deliver(i, compress(i));
            }
        };
        std::thread::scope(|scope| {
            // The calling thread works too; a helper the OS refuses (thread
            // or memory limits) only means fewer helpers, never an error.
            for _ in 1..threads {
                if std::thread::Builder::new()
                    .spawn_scoped(scope, work)
                    .is_err()
                {
                    break;
                }
            }
            work();
        });
    }
    let st = state.into_inner().expect("no worker panicked");
    debug_assert_eq!(st.next, pieces.len());
    st.out.finish()
}

struct Assembly {
    out: Assembler,
    /// Compressed pieces finished before an earlier one.
    waiting: Vec<Option<(Vec<u8>, u32, u64)>>,
    next: usize,
}

/// Writes compressed pieces, in order, as IDAT chunks of IDAT_CHUNK bytes
/// across the piece boundaries (as the streaming encoder cuts them), and
/// combines their Adler-32s for the zlib trailer.
struct Assembler {
    out: Vec<u8>,
    chunk: Vec<u8>,
    adler: u32,
}

impl Assembler {
    fn reserve(&mut self, n: usize) {
        self.out.reserve(n);
    }

    fn new(out: Vec<u8>) -> Self {
        Assembler {
            out,
            chunk: Vec::with_capacity(IDAT_CHUNK),
            adler: 1,
        }
    }

    fn piece(&mut self, (data, adler, len): (Vec<u8>, u32, u64)) {
        self.adler = adler32_combine(self.adler, adler, len);
        self.write(&data);
    }

    fn write(&mut self, mut part: &[u8]) {
        while !part.is_empty() {
            let n = (IDAT_CHUNK - self.chunk.len()).min(part.len());
            self.chunk.extend_from_slice(&part[..n]);
            part = &part[n..];
            if self.chunk.len() == IDAT_CHUNK {
                write_chunk(&mut self.out, b"IDAT", &self.chunk).expect("Vec write");
                self.chunk.clear();
            }
        }
    }

    fn finish(mut self) -> Result<Vec<u8>> {
        self.write(&zlib_trailer(self.adler));
        if !self.chunk.is_empty() {
            write_chunk(&mut self.out, b"IDAT", &self.chunk).map_err(io)?;
        }
        write_chunk(&mut self.out, b"IEND", &[]).map_err(io)?;
        Ok(self.out)
    }
}

/// Raw bytes per sample band, and the number of bands.
const SAMPLE_BAND: usize = 128 * 1024;
const SAMPLE_BANDS: usize = 4;

/// Pick the zlib tuning for `img`: the image strategy (long matches only)
/// unless zlib's own match finder makes clearly smaller output on a sample
/// of filtered rows. Photographs favour the image strategy (smaller and
/// several times faster); content whose filter residuals repeat in short
/// strings (noise shared by the three channels, dithering) favours zlib's.
/// Images up to four bands are sampled whole.
fn choose_tuning(img: &Image, level: Level) -> Tuning {
    let stride = img.stride();
    let h = img.height as usize;
    if stride == 0 || h == 0 || level.get() == 0 {
        return Tuning::Image;
    }
    let bpp = img.format.channels();
    let band_rows = (SAMPLE_BAND / stride).clamp(1, h);
    let starts: Vec<usize> = if band_rows * SAMPLE_BANDS >= h {
        vec![0]
    } else {
        (0..SAMPLE_BANDS)
            .map(|i| h * (2 * i + 1) / (2 * SAMPLE_BANDS) - band_rows / 2)
            .collect()
    };
    let rows = if starts.len() == 1 { h } else { band_rows };
    let mut sample = Vec::with_capacity(starts.len() * rows * (stride + 1));
    let zero = vec![0u8; stride];
    let mut line = vec![0u8; stride];
    for &y0 in &starts {
        for y in y0..y0 + rows {
            let row = &img.data[y * stride..(y + 1) * stride];
            let prev = if y == 0 {
                &zero[..]
            } else {
                &img.data[(y - 1) * stride..y * stride]
            };
            sample.push(filter_row(row, prev, bpp, &mut line));
            sample.extend_from_slice(&line);
        }
    }
    let size = |tuning| {
        let mut c = Compressor::zlib_tuned(level, tuning);
        let mut out = Vec::with_capacity(sample.len() / 2);
        c.write(&sample, &mut out);
        c.finish(&mut out);
        out.len()
    };
    let (image, default) = (size(Tuning::Image), size(Tuning::Default));
    // Prefer the (faster) image strategy unless zlib's saves over 3%.
    if default * 100 < image * 97 {
        Tuning::Default
    } else {
        Tuning::Image
    }
}

/// Paeth predictor (branch-free so the filter loops vectorize).
#[inline(always)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a16, b16, c16) = (i16::from(a), i16::from(b), i16::from(c));
    let pa = (b16 - c16).abs();
    let pb = (a16 - c16).abs();
    let pc = (a16 + b16 - 2 * c16).abs();
    let bc = if pb <= pc { b } else { c };
    if pa <= pb && pa <= pc { a } else { bc }
}

/// Filter one row into `out` (without the filter-type byte).
#[inline(always)]
fn apply_filter(filter: u8, row: &[u8], prev: Option<&[u8]>, bpp: usize, out: &mut [u8]) {
    let n = row.len();
    let b = bpp.min(n);
    match (filter, prev) {
        (0, _) => out.copy_from_slice(row),
        (1, _) | (4.., None) => {
            out[..b].copy_from_slice(&row[..b]);
            for i in b..n {
                out[i] = row[i].wrapping_sub(row[i - bpp]);
            }
        }
        (2, None) => out.copy_from_slice(row),
        (2, Some(p)) => {
            for i in 0..n {
                out[i] = row[i].wrapping_sub(p[i]);
            }
        }
        (3, None) => {
            out[..b].copy_from_slice(&row[..b]);
            for i in b..n {
                out[i] = row[i].wrapping_sub(row[i - bpp] >> 1);
            }
        }
        (3, Some(p)) => {
            for i in 0..b {
                out[i] = row[i].wrapping_sub(p[i] >> 1);
            }
            for i in b..n {
                out[i] =
                    row[i].wrapping_sub(((u16::from(row[i - bpp]) + u16::from(p[i])) >> 1) as u8);
            }
        }
        (_, Some(p)) => {
            for i in 0..b {
                out[i] = row[i].wrapping_sub(p[i]);
            }
            for i in b..n {
                out[i] = row[i].wrapping_sub(paeth(row[i - bpp], p[i], p[i - bpp]));
            }
        }
    }
}

photo_core::multiversion! {
    /// libpng's minimum-sum-of-absolute-differences heuristic (as Pillow
    /// uses): the five candidate sums are accumulated in one pass without
    /// storing the candidates; only the winner is then written to `out`.
    /// Ties go to the lower filter type, as before.
    fn filter_row(row: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) -> u8 = filter_row_body;
}

#[inline(always)]
fn filter_row_body(row: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) -> u8 {
    let n = row.len();
    let prev = &prev[..n];
    let b = bpp.min(n);
    let cost = |v: u8| u32::from((v as i8).unsigned_abs());
    let mut sums = [0u32; 5];
    for i in 0..b {
        let (x, up) = (row[i], prev[i]);
        sums[0] += cost(x);
        sums[1] += cost(x);
        sums[2] += cost(x.wrapping_sub(up));
        sums[3] += cost(x.wrapping_sub(up >> 1));
        sums[4] += cost(x.wrapping_sub(up));
    }
    // Per-chunk u16 accumulators (at most 128 per byte, so 256 bytes per
    // chunk cannot overflow) keep the main loop in 16-bit lanes.
    let mut i = b;
    while i < n {
        let end = (i + 256).min(n);
        let mut acc = [0u16; 5];
        for j in i..end {
            let (x, a, up, c) = (row[j], row[j - bpp], prev[j], prev[j - bpp]);
            let c16 = |v: u8| u16::from((v as i8).unsigned_abs());
            acc[0] += c16(x);
            acc[1] += c16(x.wrapping_sub(a));
            acc[2] += c16(x.wrapping_sub(up));
            acc[3] += c16(x.wrapping_sub(((u16::from(a) + u16::from(up)) >> 1) as u8));
            acc[4] += c16(x.wrapping_sub(paeth(a, up, c)));
        }
        for k in 0..5 {
            sums[k] += u32::from(acc[k]);
        }
        i = end;
    }
    let mut best = 0;
    for k in 1..5 {
        if sums[k] < sums[best] {
            best = k;
        }
    }
    apply_filter(best as u8, row, Some(prev), bpp, out);
    best as u8
}
