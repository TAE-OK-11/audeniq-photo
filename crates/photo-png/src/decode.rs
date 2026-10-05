//! PNG decoding to 8-bit interleaved pixels.

use crate::{ChunkIter, Info, MAX_TEXT_TOTAL, parse_text};
use photo_core::{Deadline, Error, Image, Limits, PixelFormat, Result};
use std::borrow::Cow;

struct Parsed<'a> {
    info: Info,
    idat: Vec<&'a [u8]>,
}

fn parse(data: &[u8], keep_idat: bool) -> Result<Parsed<'_>> {
    let mut info: Option<Info> = None;
    let mut idat = Vec::new();
    let mut seen_idat = false;
    let mut idat_ended = false;
    let mut seen_iend = false;
    let mut text_budget = MAX_TEXT_TOTAL;
    for chunk in ChunkIter::new(data)? {
        let chunk = chunk?;
        let kind = &chunk.kind;
        if info.is_none() {
            if kind != b"IHDR" {
                return Err(Error::Invalid("IHDR must be first"));
            }
            info = Some(Info::parse_ihdr(chunk.data)?);
            continue;
        }
        let i = info.as_mut().expect("IHDR parsed");
        if kind != b"IDAT" && seen_idat {
            idat_ended = true;
        }
        match kind {
            b"IHDR" => return Err(Error::Invalid("duplicate IHDR")),
            b"PLTE" => {
                if seen_idat || !i.palette.is_empty() {
                    return Err(Error::Invalid("PLTE order"));
                }
                if chunk.data.len() % 3 != 0 || chunk.data.is_empty() || chunk.data.len() > 768 {
                    return Err(Error::Invalid("PLTE length"));
                }
                if i.color_type == 0 || i.color_type == 4 {
                    return Err(Error::Invalid("PLTE in grayscale image"));
                }
                i.palette = chunk
                    .data
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2]])
                    .collect();
            }
            b"tRNS" => {
                let ok = match i.color_type {
                    0 => chunk.data.len() == 2,
                    2 => chunk.data.len() == 6,
                    3 => chunk.data.len() <= 256,
                    _ => false,
                };
                // Invalid tRNS is ancillary: ignore it like libpng does.
                if ok && !seen_idat {
                    i.transparency = Some(chunk.data.to_vec());
                }
            }
            b"IDAT" => {
                if idat_ended {
                    return Err(Error::Invalid("non-consecutive IDAT"));
                }
                if i.color_type == 3 && i.palette.is_empty() {
                    return Err(Error::Invalid("missing PLTE"));
                }
                seen_idat = true;
                if keep_idat {
                    idat.push(chunk.data);
                }
            }
            b"IEND" => {
                seen_iend = true;
            }
            b"iCCP" => {
                if i.icc_profile.is_none()
                    && !seen_idat
                    && let Some(nul) = chunk.data.iter().position(|&b| b == 0)
                {
                    let body = &chunk.data[nul + 1..];
                    if body.first() == Some(&0) {
                        let mut profile = Vec::new();
                        // ICC profiles are small; 4 MiB is generous.
                        if photo_deflate::inflate_zlib(&body[1..], &mut profile, 4 << 20, false)
                            .is_ok()
                        {
                            i.icc_name =
                                Some(chunk.data[..nul].iter().map(|&b| char::from(b)).collect());
                            i.icc_profile = Some(profile);
                        }
                    }
                }
            }
            b"sRGB" => {
                if let [intent] = chunk.data {
                    i.srgb_intent = Some(*intent);
                }
            }
            b"eXIf" => {
                if i.exif.is_none() {
                    i.exif = Some(chunk.data.to_vec());
                }
            }
            b"tEXt" | b"zTXt" | b"iTXt" => {
                // Malformed text is ancillary; skip it but keep decoding.
                match parse_text(kind, chunk.data, &mut text_budget) {
                    Ok(t) => i.texts.push(t),
                    Err(Error::Limit(_)) => return Err(Error::Limit("PNG text size")),
                    Err(_) => {}
                }
            }
            b"acTL" => {
                if chunk.data.len() == 8 {
                    i.animation_frames = Some(u32::from_be_bytes([
                        chunk.data[0],
                        chunk.data[1],
                        chunk.data[2],
                        chunk.data[3],
                    ]));
                }
            }
            _ => {
                if kind[0].is_ascii_uppercase() {
                    return Err(Error::Unsupported("unknown critical chunk"));
                }
            }
        }
        if seen_iend {
            break;
        }
    }
    let info = info.ok_or(Error::Truncated)?;
    if !seen_idat {
        return Err(Error::Invalid("missing IDAT"));
    }
    if !seen_iend {
        return Err(Error::Truncated);
    }
    Ok(Parsed { info, idat })
}

/// Read header and metadata only (no pixel decompression).
pub fn read_info(data: &[u8]) -> Result<Info> {
    parse(data, false).map(|p| p.info)
}

const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn pass_size(full: usize, start: usize, step: usize) -> usize {
    if start >= full {
        0
    } else {
        (full - start).div_ceil(step)
    }
}

fn row_bytes(width: usize, info: &Info) -> usize {
    (width * info.channels() * info.bit_depth as usize).div_ceil(8)
}

#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Undo one row's filter in place. `prev` is the unfiltered previous row.
pub(crate) fn unfilter(filter: u8, bpp: usize, prev: Option<&[u8]>, cur: &mut [u8]) -> Result<()> {
    let n = cur.len();
    match (filter, prev) {
        (0, _) => {}
        (1, _) => {
            for i in bpp..n {
                cur[i] = cur[i].wrapping_add(cur[i - bpp]);
            }
        }
        (2, None) => {}
        (2, Some(p)) => {
            for (c, &u) in cur.iter_mut().zip(p) {
                *c = c.wrapping_add(u);
            }
        }
        (3, None) => {
            for i in bpp..n {
                cur[i] = cur[i].wrapping_add(cur[i - bpp] >> 1);
            }
        }
        (3, Some(p)) => {
            for i in 0..bpp.min(n) {
                cur[i] = cur[i].wrapping_add(p[i] >> 1);
            }
            for i in bpp..n {
                let avg = ((u16::from(cur[i - bpp]) + u16::from(p[i])) >> 1) as u8;
                cur[i] = cur[i].wrapping_add(avg);
            }
        }
        (4, None) => {
            // Paeth with a zero row above degenerates to Sub.
            for i in bpp..n {
                cur[i] = cur[i].wrapping_add(cur[i - bpp]);
            }
        }
        (4, Some(p)) => {
            for i in 0..bpp.min(n) {
                cur[i] = cur[i].wrapping_add(p[i]);
            }
            for i in bpp..n {
                cur[i] = cur[i].wrapping_add(paeth(cur[i - bpp], p[i], p[i - bpp]));
            }
        }
        _ => return Err(Error::Invalid("filter type")),
    }
    Ok(())
}

fn out_format(info: &Info) -> PixelFormat {
    match info.color_type {
        0 => PixelFormat::Gray8,
        2 => PixelFormat::Rgb8,
        3 if info.transparency.is_some() => PixelFormat::Rgba8,
        3 => PixelFormat::Rgb8,
        4 => PixelFormat::GrayAlpha8,
        _ => PixelFormat::Rgba8,
    }
}

/// Expand one unfiltered row of `w` pixels into 8-bit samples of `out_format`.
fn expand(info: &Info, row: &[u8], w: usize, out: &mut Vec<u8>) {
    out.clear();
    let depth = info.bit_depth as usize;
    match (info.color_type, depth) {
        (2 | 4 | 6, 8) | (0, 8) => out.extend_from_slice(&row[..w * info.channels()]),
        (_, 16) => out.extend(
            row.as_chunks::<2>()
                .0
                .iter()
                .take(w * info.channels())
                .map(|s| s[0]),
        ),
        (0, d) => {
            let scale = 255 / ((1u16 << d) - 1) as u8;
            out.extend((0..w).map(|x| sample(row, x, d) * scale));
        }
        (3, d) => {
            let alpha = info.transparency.as_deref();
            for x in 0..w {
                let idx = if d == 8 { row[x] } else { sample(row, x, d) } as usize;
                // Out-of-range indices read as black, like Pillow's padded palette.
                let rgb = info.palette.get(idx).copied().unwrap_or([0, 0, 0]);
                out.extend_from_slice(&rgb);
                if let Some(a) = alpha {
                    out.push(a.get(idx).copied().unwrap_or(255));
                }
            }
        }
        _ => unreachable!("validated in IHDR"),
    }
}

#[inline]
fn sample(row: &[u8], x: usize, depth: usize) -> u8 {
    let bit = x * depth;
    let byte = row[bit / 8];
    let shift = 8 - depth - (bit % 8);
    (byte >> shift) & ((1u8 << depth) - 1)
}

/// Decode a PNG to 8-bit pixels. 16-bit samples keep their high byte,
/// palettes expand to RGB (RGBA when tRNS is present).
pub fn decode(data: &[u8], limits: &Limits, deadline: &Deadline) -> Result<(Info, Image)> {
    let Parsed { info, idat } = parse(data, true)?;
    limits.check_dimensions(info.width, info.height)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let passes: Vec<(usize, usize, usize, usize, usize, usize)> = if info.interlace {
        ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| (x0, y0, dx, dy, pass_size(w, x0, dx), pass_size(h, y0, dy)))
            .filter(|p| p.4 > 0 && p.5 > 0)
            .collect()
    } else {
        vec![(0, 0, 1, 1, w, h)]
    };
    let mut raw_len: u64 = 0;
    for p in &passes {
        raw_len += p.5 as u64 * (1 + row_bytes(p.4, &info) as u64);
    }
    let raw_len = limits.alloc_size(raw_len, 1)?;
    let fmt = out_format(&info);
    let out_len = limits.alloc_size((w * h) as u64, fmt.channels() as u64)?;

    let stream: Cow<[u8]> = if idat.len() == 1 {
        Cow::Borrowed(idat[0])
    } else {
        Cow::Owned(idat.concat())
    };
    let mut raw = Vec::with_capacity(raw_len);
    let r = photo_deflate::inflate_zlib(&stream, &mut raw, raw_len, true)?;
    drop(stream);
    if raw.len() != raw_len || (r.complete && raw.len() < raw_len) {
        return Err(Error::Truncated);
    }
    let bpp = (info.channels() * info.bit_depth as usize).div_ceil(8);

    // Fast path: 8-bit, non-interlaced, non-palette. Unfilter in place and
    // squeeze out the filter bytes, so the decoded image reuses `raw`.
    if !info.interlace && info.bit_depth == 8 && info.color_type != 3 {
        let rb = row_bytes(w, &info);
        for y in 0..h {
            if y % 64 == 0 {
                deadline.check()?;
            }
            let start = y * (rb + 1);
            let filter = raw[start];
            if y == 0 {
                unfilter(filter, bpp, None, &mut raw[1..1 + rb])?;
            } else {
                let (before, cur) = raw.split_at_mut(start);
                let prev = &before[start - rb..];
                unfilter(filter, bpp, Some(prev), &mut cur[1..1 + rb])?;
            }
        }
        for y in 0..h {
            raw.copy_within(y * (rb + 1) + 1..(y + 1) * (rb + 1), y * rb);
        }
        raw.truncate(h * rb);
        raw.shrink_to_fit();
        let (width, height) = (info.width, info.height);
        return Ok((
            info,
            Image {
                width,
                height,
                format: fmt,
                data: raw,
            },
        ));
    }

    let ch = fmt.channels();
    let mut out = vec![0u8; out_len];
    let mut scratch = Vec::with_capacity(w * ch);
    let mut offset = 0;
    for &(x0, y0, dx, dy, pw, ph) in &passes {
        let rb = row_bytes(pw, &info);
        for py in 0..ph {
            if py % 64 == 0 {
                deadline.check()?;
            }
            let start = offset + py * (rb + 1);
            let filter = raw[start];
            if py == 0 {
                unfilter(filter, bpp, None, &mut raw[start + 1..start + 1 + rb])?;
            } else {
                let (before, cur) = raw.split_at_mut(start);
                unfilter(
                    filter,
                    bpp,
                    Some(&before[start - rb..]),
                    &mut cur[1..1 + rb],
                )?;
            }
            expand(&info, &raw[start + 1..start + 1 + rb], pw, &mut scratch);
            let y = y0 + py * dy;
            let row = &mut out[y * w * ch..(y + 1) * w * ch];
            if dx == 1 {
                row.copy_from_slice(&scratch);
            } else {
                for (px, src) in scratch.chunks_exact(ch).enumerate() {
                    let x = x0 + px * dx;
                    row[x * ch..x * ch + ch].copy_from_slice(src);
                }
            }
        }
        offset += ph * (rb + 1);
    }
    drop(raw);
    let (width, height) = (info.width, info.height);
    Ok((
        info,
        Image {
            width,
            height,
            format: fmt,
            data: out,
        },
    ))
}
