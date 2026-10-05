//! Marker-driven JPEG decoding: sequential (interleaved scans decode
//! straight into sample planes) and buffered (progressive or multi-scan,
//! coefficients kept until the end, as libjpeg's full-image buffer).

use crate::ZIGZAG;
use crate::color::{Plane, convert};
use crate::huffman::{BitReader, HuffTable};
use crate::idct::idct_islow;
use crate::markers::{ColorTransform, FrameInfo, Info, assemble_icc, color_transform, next_marker};
use photo_core::{Bytes, Deadline, Error, Image, Limits, Result};

/// Progressive files with more scans than this are refused (scan bombs).
const MAX_SCANS: usize = 128;

struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks per row/column in the padded (whole-MCU) layout.
    bw: usize,
    bh: usize,
    /// Real downsampled size in samples.
    dw: usize,
    dh: usize,
    quant: Option<[u16; 64]>,
    coefs: Vec<i16>,
    plane: Vec<u8>,
    dc_pred: i32,
}

struct Frame {
    info: FrameInfo,
    comps: Vec<Comp>,
    max_h: usize,
    max_v: usize,
    mcus_x: usize,
    mcus_y: usize,
    progressive: bool,
    buffered: Option<bool>,
}

struct State {
    qt: [Option<[u16; 64]>; 4],
    dc: [Option<HuffTable>; 4],
    ac: [Option<HuffTable>; 4],
    restart: usize,
    frame: Option<Frame>,
    scans: usize,
    eobrun: u32,
}

fn parse_dqt(data: &[u8], qt: &mut [Option<[u16; 64]>; 4]) -> Result<()> {
    let mut b = Bytes::new(data);
    while b.remaining() > 0 {
        let pq_tq = b.u8()?;
        let (pq, tq) = (pq_tq >> 4, (pq_tq & 15) as usize);
        if tq > 3 || pq > 1 {
            return Err(Error::Invalid("DQT table"));
        }
        let mut t = [0u16; 64];
        for k in 0..64 {
            t[ZIGZAG[k]] = if pq == 0 { u16::from(b.u8()?) } else { b.u16_be()? };
        }
        qt[tq] = Some(t);
    }
    Ok(())
}

fn parse_dht(data: &[u8], dc: &mut [Option<HuffTable>; 4], ac: &mut [Option<HuffTable>; 4]) -> Result<()> {
    let mut b = Bytes::new(data);
    while b.remaining() > 0 {
        let tc_th = b.u8()?;
        let (tc, th) = (tc_th >> 4, (tc_th & 15) as usize);
        if tc > 1 || th > 3 {
            return Err(Error::Invalid("DHT table"));
        }
        let counts: [u8; 16] = b.array()?;
        let n: usize = counts.iter().map(|&c| c as usize).sum();
        let symbols = b.take(n)?;
        let t = HuffTable::new(&counts, symbols)?;
        if tc == 0 {
            dc[th] = Some(t);
        } else {
            ac[th] = Some(t);
        }
    }
    Ok(())
}

impl Frame {
    fn new(info: FrameInfo, limits: &Limits) -> Result<Frame> {
        match info.marker {
            0xC0 | 0xC1 | 0xC2 => {}
            0xC3 | 0xC7 | 0xCB | 0xCF => return Err(Error::Unsupported("lossless JPEG")),
            0xC9..=0xCF => return Err(Error::Unsupported("arithmetic-coded JPEG")),
            _ => return Err(Error::Unsupported("hierarchical JPEG")),
        }
        if info.precision != 8 {
            return Err(Error::Unsupported("JPEG sample precision other than 8 bits"));
        }
        if info.height == 0 {
            return Err(Error::Unsupported("JPEG DNL marker"));
        }
        if info.components.len() == 2 {
            return Err(Error::Unsupported("two-component JPEG"));
        }
        limits.check_dimensions(info.width, info.height)?;
        let max_h = info.components.iter().map(|c| c.h as usize).max().unwrap_or(1);
        let max_v = info.components.iter().map(|c| c.v as usize).max().unwrap_or(1);
        let (w, h) = (info.width as usize, info.height as usize);
        let mcus_x = w.div_ceil(8 * max_h);
        let mcus_y = h.div_ceil(8 * max_v);
        let mut comps = Vec::new();
        for c in &info.components {
            let (ch, cv) = (c.h as usize, c.v as usize);
            if max_h % ch != 0 || max_v % cv != 0 {
                return Err(Error::Unsupported("non-integral sampling ratio"));
            }
            comps.push(Comp {
                id: c.id,
                h: ch,
                v: cv,
                tq: c.tq as usize,
                bw: mcus_x * ch,
                bh: mcus_y * cv,
                dw: (w * ch).div_ceil(max_h),
                dh: (h * cv).div_ceil(max_v),
                quant: None,
                coefs: Vec::new(),
                plane: Vec::new(),
                dc_pred: 0,
            });
        }
        let progressive = info.progressive();
        Ok(Frame { info, comps, max_h, max_v, mcus_x, mcus_y, progressive, buffered: None })
    }

    fn allocate(&mut self, buffered: bool, limits: &Limits) -> Result<()> {
        let mut total: u64 = 0;
        for c in &self.comps {
            total += (c.bw * c.bh * 64) as u64 * if buffered { 2 } else { 1 };
        }
        limits.alloc_size(total, 1)?;
        for c in &mut self.comps {
            if buffered {
                c.coefs = vec![0; c.bw * c.bh * 64];
            } else {
                c.plane = vec![0; c.bw * c.bh * 64];
            }
        }
        self.buffered = Some(buffered);
        Ok(())
    }
}

struct ScanComp {
    index: usize,
    dc: usize,
    ac: usize,
}

/// Decode a JPEG to 8-bit pixels: Gray8, Rgb8 or Cmyk8 (Adobe-inverted
/// back to normal CMYK, as Pillow presents it).
pub fn decode(data: &[u8], limits: &Limits, deadline: &Deadline) -> Result<(Info, Image)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(Error::Invalid("not a JPEG file"));
    }
    let mut st = State { qt: [None; 4], dc: Default::default(), ac: Default::default(), restart: 0, frame: None, scans: 0, eobrun: 0 };
    let mut jfif = false;
    let mut adobe = None;
    let mut icc_chunks = Vec::new();
    let (mut exif, mut xmp) = (None, None);
    let mut comments = Vec::new();
    let mut pos = 2;
    loop {
        let (marker, at) = match next_marker(data, pos) {
            Ok(m) => m,
            // Missing EOI after complete scans: libjpeg warns, Pillow loads.
            Err(Error::Truncated) if st.scans > 0 => break,
            Err(e) => return Err(e),
        };
        match marker {
            0xD9 => break,
            0xD8 => return Err(Error::Invalid("unexpected SOI")),
            0x01 | 0xD0..=0xD7 => {
                pos = at + 2;
                continue;
            }
            _ => {}
        }
        let mut b = Bytes::at(data, at + 2);
        let len = b.u16_be()? as usize;
        if len < 2 {
            return Err(Error::Invalid("segment length"));
        }
        let body = b.take(len - 2)?;
        pos = at + 2 + len;
        match marker {
            0xDB => parse_dqt(body, &mut st.qt)?,
            0xC4 => parse_dht(body, &mut st.dc, &mut st.ac)?,
            0xDD => {
                if body.len() != 2 {
                    return Err(Error::Invalid("DRI length"));
                }
                st.restart = usize::from(u16::from_be_bytes([body[0], body[1]]));
            }
            0xC0..=0xCF => {
                if st.frame.is_some() {
                    return Err(Error::Invalid("multiple SOF markers"));
                }
                st.frame = Some(Frame::new(FrameInfo::parse(marker, body)?, limits)?);
            }
            0xDA => {
                st.scans += 1;
                if st.scans > MAX_SCANS {
                    return Err(Error::Limit("JPEG scan count"));
                }
                pos = scan(data, body, pos, &mut st, limits, deadline)?;
            }
            0xDC => return Err(Error::Unsupported("JPEG DNL marker")),
            0xE0 if body.starts_with(b"JFIF\0") => jfif = true,
            0xE1 if body.starts_with(b"Exif\0") && body.len() > 6 && exif.is_none() => exif = Some(body[6..].to_vec()),
            0xE1 if body.starts_with(b"http://ns.adobe.com/xap/1.0/\0") && xmp.is_none() => xmp = Some(body[29..].to_vec()),
            0xE2 if body.starts_with(b"ICC_PROFILE\0") && body.len() >= 14 => {
                icc_chunks.push((body[12], body[13], &body[14..]))
            }
            0xEE if body.starts_with(b"Adobe") && body.len() >= 12 => adobe = Some(body[11]),
            0xFE => comments.push(body.to_vec()),
            _ => {}
        }
    }
    let mut frame = st.frame.take().ok_or(Error::Invalid("missing SOF"))?;
    if st.scans == 0 {
        return Err(Error::Invalid("no scans"));
    }
    let transform = color_transform(&frame.info, jfif, adobe);
    if frame.buffered == Some(true) {
        for c in &mut frame.comps {
            deadline.check()?;
            let quant = c.quant.ok_or(Error::Invalid("component never scanned"))?;
            let stride = c.bw * 8;
            let mut plane = vec![0u8; c.bw * c.bh * 64];
            let mut block = [0i16; 64];
            for by in 0..c.bh {
                for bx in 0..c.bw {
                    let i = (by * c.bw + bx) * 64;
                    block.copy_from_slice(&c.coefs[i..i + 64]);
                    idct_islow(&block, &quant, &mut plane[by * 8 * stride + bx * 8..], stride);
                }
            }
            c.coefs = Vec::new();
            c.plane = plane;
        }
    }
    let (w, h) = (frame.info.width as usize, frame.info.height as usize);
    let (max_h, max_v) = (frame.max_h, frame.max_v);
    let planes: Vec<Plane> = frame
        .comps
        .iter_mut()
        .map(|c| Plane {
            data: std::mem::take(&mut c.plane),
            stride: c.bw * 8,
            width: c.dw,
            height: c.dh,
            h_ratio: max_h / c.h,
            v_ratio: max_v / c.v,
        })
        .collect();
    let image = convert(&planes, transform, w, h, deadline)?;
    drop(planes);
    let info = Info {
        icc_profile: assemble_icc(&mut icc_chunks),
        color: transform,
        frame: frame.info,
        jfif,
        adobe_transform: adobe,
        exif,
        xmp,
        comments,
    };
    if matches!(transform, ColorTransform::Gray) && image.format != photo_core::PixelFormat::Gray8 {
        return Err(Error::Invalid("color transform"));
    }
    Ok((info, image))
}

/// Decode one scan. Returns the offset to continue marker parsing from.
fn scan(data: &[u8], header: &[u8], start: usize, st: &mut State, limits: &Limits, deadline: &Deadline) -> Result<usize> {
    let frame = st.frame.as_mut().ok_or(Error::Invalid("SOS before SOF"))?;
    let mut b = Bytes::new(header);
    let ns = b.u8()? as usize;
    if ns == 0 || ns > 4 || header.len() != 4 + 2 * ns {
        return Err(Error::Invalid("SOS length"));
    }
    let mut sc = Vec::with_capacity(ns);
    for _ in 0..ns {
        let id = b.u8()?;
        let t = b.u8()?;
        let index = frame.comps.iter().position(|c| c.id == id).ok_or(Error::Invalid("SOS component"))?;
        if sc.iter().any(|s: &ScanComp| s.index == index) {
            return Err(Error::Invalid("duplicate SOS component"));
        }
        let (dc, ac) = ((t >> 4) as usize, (t & 15) as usize);
        if dc > 3 || ac > 3 {
            return Err(Error::Invalid("SOS table selector"));
        }
        sc.push(ScanComp { index, dc, ac });
    }
    let ss = b.u8()? as usize;
    let se = b.u8()? as usize;
    let ahal = b.u8()?;
    let (ah, al) = (u32::from(ahal >> 4), u32::from(ahal & 15));
    if frame.progressive {
        if ss > se || se > 63 || (ss == 0 && se != 0) || (ss > 0 && ns != 1) || al > 13 || ah > 13 {
            return Err(Error::Invalid("progressive scan parameters"));
        }
    }
    if ns > 1 && sc.iter().map(|s| frame.comps[s.index].h * frame.comps[s.index].v).sum::<usize>() > 10 {
        return Err(Error::Invalid("too many blocks per MCU"));
    }
    if frame.buffered.is_none() {
        let direct = !frame.progressive && ns == frame.comps.len();
        frame.allocate(!direct, limits)?;
    } else if frame.buffered == Some(false) {
        return Err(Error::Invalid("extra scan in single-scan sequential JPEG"));
    }
    let buffered = frame.buffered == Some(true);
    for s in &sc {
        let c = &mut frame.comps[s.index];
        if c.quant.is_none() {
            c.quant = Some(st.qt[c.tq].ok_or(Error::Invalid("missing quantization table"))?);
        }
        c.dc_pred = 0;
        let need_dc = !frame.progressive || (ss == 0 && ah == 0);
        let need_ac = !frame.progressive || ss > 0;
        if need_dc && st.dc[s.dc].is_none() {
            return Err(Error::Invalid("missing DC Huffman table"));
        }
        if need_ac && st.ac[s.ac].is_none() {
            return Err(Error::Invalid("missing AC Huffman table"));
        }
    }
    st.eobrun = 0;

    let mut r = BitReader::new(data, start);
    let (units_x, units_y) = if ns == 1 {
        let c = &frame.comps[sc[0].index];
        (c.dw.div_ceil(8), c.dh.div_ceil(8))
    } else {
        (frame.mcus_x, frame.mcus_y)
    };
    let restart = st.restart;
    let mut rst_count = 0u8;
    let mut todo = restart;
    let mut block = [0i16; 64];
    for uy in 0..units_y {
        if uy % 8 == 0 {
            deadline.check()?;
        }
        for ux in 0..units_x {
            if restart > 0 {
                if todo == 0 {
                    r.restart(rst_count)?;
                    rst_count = rst_count.wrapping_add(1);
                    todo = restart;
                    for s in &sc {
                        frame.comps[s.index].dc_pred = 0;
                    }
                    st.eobrun = 0;
                }
                todo -= 1;
            }
            for s in &sc {
                let c = &mut frame.comps[s.index];
                let (bh, bv) = if ns == 1 { (1, 1) } else { (c.h, c.v) };
                for v in 0..bv {
                    for h in 0..bh {
                        let (bx, by) = if ns == 1 { (ux, uy) } else { (ux * c.h + h, uy * c.v + v) };
                        if buffered {
                            let i = (by * c.bw + bx) * 64;
                            let coefs: &mut [i16; 64] = (&mut c.coefs[i..i + 64]).try_into().expect("64");
                            if frame.progressive {
                                if ss == 0 {
                                    if ah == 0 {
                                        let t = st.dc[s.dc].as_ref().expect("checked");
                                        let s0 = u32::from(r.decode(t)?);
                                        let diff = r.receive_extend(s0);
                                        c.dc_pred = c.dc_pred.wrapping_add(diff);
                                        coefs[0] = (c.dc_pred << al) as i16;
                                    } else if r.bit() == 1 {
                                        coefs[0] |= (1 << al) as i16;
                                    }
                                } else if ah == 0 {
                                    ac_first(&mut r, st.ac[s.ac].as_ref().expect("checked"), ss, se, al, &mut st.eobrun, coefs)?;
                                } else {
                                    ac_refine(&mut r, st.ac[s.ac].as_ref().expect("checked"), ss, se, al, &mut st.eobrun, coefs)?;
                                }
                            } else {
                                sequential(&mut r, st.dc[s.dc].as_ref().expect("checked"), st.ac[s.ac].as_ref().expect("checked"), &mut c.dc_pred, coefs)?;
                            }
                        } else {
                            sequential(&mut r, st.dc[s.dc].as_ref().expect("checked"), st.ac[s.ac].as_ref().expect("checked"), &mut c.dc_pred, &mut block)?;
                            let stride = c.bw * 8;
                            let q = c.quant.as_ref().expect("latched");
                            idct_islow(&block, q, &mut c.plane[by * 8 * stride + bx * 8..], stride);
                        }
                    }
                }
            }
            if r.overrun() {
                return Err(Error::Truncated);
            }
        }
    }
    let (m, at) = match r.finish_segment() {
        Ok(v) => v,
        Err(Error::Truncated) => return Ok(data.len()),
        Err(e) => return Err(e),
    };
    let _ = m;
    Ok(at)
}

fn sequential(r: &mut BitReader, dc: &HuffTable, ac: &HuffTable, pred: &mut i32, block: &mut [i16; 64]) -> Result<()> {
    *block = [0; 64];
    let s = u32::from(r.decode(dc)?);
    let diff = r.receive_extend(s);
    *pred = pred.wrapping_add(diff);
    block[0] = *pred as i16;
    let mut k = 1;
    while k < 64 {
        let rs = r.decode(ac)?;
        let (run, s) = (usize::from(rs >> 4), u32::from(rs & 15));
        if s != 0 {
            k += run;
            let v = r.receive_extend(s);
            block[ZIGZAG[k]] = v as i16;
        } else if run != 15 {
            break;
        } else {
            k += 15;
        }
        k += 1;
    }
    Ok(())
}

fn ac_first(r: &mut BitReader, t: &HuffTable, ss: usize, se: usize, al: u32, eobrun: &mut u32, block: &mut [i16; 64]) -> Result<()> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = ss;
    while k <= se {
        let rs = r.decode(t)?;
        let (run, s) = (u32::from(rs >> 4), u32::from(rs & 15));
        if s != 0 {
            k += run as usize;
            let v = r.receive_extend(s);
            block[ZIGZAG[k]] = (v << al) as i16;
        } else if run == 15 {
            k += 15;
        } else {
            *eobrun = 1 << run;
            if run > 0 {
                *eobrun += r.bits(run);
            }
            *eobrun -= 1;
            break;
        }
        k += 1;
    }
    Ok(())
}

fn ac_refine(r: &mut BitReader, t: &HuffTable, ss: usize, se: usize, al: u32, eobrun: &mut u32, block: &mut [i16; 64]) -> Result<()> {
    let p1: i16 = 1 << al;
    let m1: i16 = (-1i32 << al) as i16;
    let mut k = ss;
    let refine = |r: &mut BitReader, c: &mut i16| {
        if r.bit() == 1 && (*c & p1) == 0 {
            *c = if *c >= 0 { c.wrapping_add(p1) } else { c.wrapping_add(m1) };
        }
    };
    if *eobrun == 0 {
        while k <= se {
            let rs = r.decode(t)?;
            let (mut run, s) = (i32::from(rs >> 4), rs & 15);
            let mut value = 0i16;
            if s != 0 {
                value = if r.bit() == 1 { p1 } else { m1 };
            } else if run != 15 {
                *eobrun = 1 << run;
                if run > 0 {
                    *eobrun += r.bits(run as u32);
                }
                break;
            }
            while k <= se {
                let c = &mut block[ZIGZAG[k]];
                if *c != 0 {
                    refine(r, c);
                } else {
                    run -= 1;
                    if run < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if value != 0 {
                block[ZIGZAG[k]] = value;
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        while k <= se {
            let c = &mut block[ZIGZAG[k]];
            if *c != 0 {
                refine(r, c);
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Ok(())
}
