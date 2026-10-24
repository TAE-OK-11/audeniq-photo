//! Marker-driven JPEG decoding: sequential (one interleaved scan streams
//! MCU rows through three-band sample rings into the output image) and
//! buffered (progressive or multi-scan, coefficients kept until the end, as
//! libjpeg's full-image buffer).

use crate::ZIGZAG;
use crate::color::{Converter, Plane, RING, convert, output_format};
use crate::huffman::{BitReader, HuffTable};
use crate::idct::idct_islow;
use crate::markers::{ColorTransform, FrameInfo, Info, assemble_icc, color_transform, next_marker};
use crate::turn;
use photo_core::{Bytes, Deadline, Error, Image, Limits, PixelFormat, Result};

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
    stream: Option<Stream>,
}

/// Output of a single-scan sequential frame, filled MCU row by MCU row.
struct Stream {
    /// Ring planes of the components that reach the output.
    planes: Vec<Plane>,
    conv: Converter,
    format: PixelFormat,
    data: Vec<u8>,
    /// Output rows per row group (MCU row, or block row of a one-component
    /// scan).
    rows: usize,
    /// Axes-swapping orientation (5..=8) or 0; rows are converted into
    /// `band` and placed turned.
    turn: u8,
    band: Vec<u8>,
}

struct State {
    qt: [Option<[u16; 64]>; 4],
    dc: [Option<HuffTable>; 4],
    ac: [Option<HuffTable>; 4],
    restart: usize,
    frame: Option<Frame>,
    scans: usize,
    eobrun: u32,
    /// Only component 0 needs pixels (luma decode of YCbCr/YCCK).
    luma_only: bool,
    jfif: bool,
    adobe: Option<u8>,
    opts: DecodeOptions,
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
            t[ZIGZAG[k]] = if pq == 0 {
                u16::from(b.u8()?)
            } else {
                b.u16_be()?
            };
        }
        qt[tq] = Some(t);
    }
    Ok(())
}

fn parse_dht(
    data: &[u8],
    dc: &mut [Option<HuffTable>; 4],
    ac: &mut [Option<HuffTable>; 4],
) -> Result<()> {
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
            0xC0..=0xC2 => {}
            0xC3 | 0xC7 | 0xCB | 0xCF => return Err(Error::Unsupported("lossless JPEG")),
            0xC9..=0xCF => return Err(Error::Unsupported("arithmetic-coded JPEG")),
            _ => return Err(Error::Unsupported("hierarchical JPEG")),
        }
        if info.precision != 8 {
            return Err(Error::Unsupported(
                "JPEG sample precision other than 8 bits",
            ));
        }
        if info.height == 0 {
            return Err(Error::Unsupported("JPEG DNL marker"));
        }
        if info.components.len() == 2 {
            return Err(Error::Unsupported("two-component JPEG"));
        }
        limits.check_dimensions(info.width, info.height)?;
        let max_h = info
            .components
            .iter()
            .map(|c| c.h as usize)
            .max()
            .unwrap_or(1);
        let max_v = info
            .components
            .iter()
            .map(|c| c.v as usize)
            .max()
            .unwrap_or(1);
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
        Ok(Frame {
            info,
            comps,
            max_h,
            max_v,
            mcus_x,
            mcus_y,
            progressive,
            buffered: None,
            stream: None,
        })
    }

    /// Buffered frames keep every coefficient; a direct frame decodes into
    /// rings of three row groups per component and converts as it goes.
    fn allocate(
        &mut self,
        buffered: bool,
        output: Option<(ColorTransform, bool)>,
        turn: u8,
        limits: &Limits,
    ) -> Result<()> {
        self.buffered = Some(buffered);
        if buffered {
            let total: u64 = self
                .comps
                .iter()
                .map(|c| (c.bw * c.bh * 64) as u64 * 2)
                .sum();
            limits.alloc_size(total, 1)?;
            for c in &mut self.comps {
                c.coefs = vec![0; c.bw * c.bh * 64];
            }
            return Ok(());
        }
        let (transform, invert) = output.expect("direct frames know their output");
        let format = output_format(transform);
        let (w, h) = (self.info.width as usize, self.info.height as usize);
        let one = self.comps.len() == 1;
        let planes: Vec<Plane> = self.comps[..format.channels().min(self.comps.len())]
            .iter()
            .map(|c| {
                let band = if one { 8 } else { 8 * c.v };
                Plane {
                    data: vec![0; RING * band * c.bw * 8],
                    stride: c.bw * 8,
                    width: c.dw,
                    height: c.dh,
                    h_ratio: self.max_h / c.h,
                    v_ratio: self.max_v / c.v,
                    band,
                }
            })
            .collect();
        let rings: usize = planes.iter().map(|p| p.data.len()).sum();
        limits.alloc_size((w * h * format.channels() + rings) as u64, 1)?;
        let conv = Converter::new(&planes, transform, invert, w)?;
        let rows = if one { 8 } else { 8 * self.max_v };
        self.stream = Some(Stream {
            planes,
            conv,
            format,
            data: vec![0; w * h * format.channels()],
            rows,
            turn,
            band: if turn != 0 {
                vec![0; rows * w * format.channels()]
            } else {
                Vec::new()
            },
        });
        Ok(())
    }
}

impl Stream {
    /// Convert row group `g`; its neighbours must still be in the rings.
    fn emit(&mut self, g: usize, width: usize, height: usize) {
        let y0 = g * self.rows;
        let y1 = ((g + 1) * self.rows).min(height);
        if y0 >= y1 {
            return;
        }
        let ch = self.format.channels();
        let row = width * ch;
        if self.turn == 0 {
            self.conv
                .rows(&self.planes, y0, &mut self.data[y0 * row..y1 * row]);
        } else {
            let band = &mut self.band[..(y1 - y0) * row];
            self.conv.rows(&self.planes, y0, band);
            turn::place(band, y0, width, height, ch, self.turn, &mut self.data);
        }
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
    decode_with(data, limits, deadline, &DecodeOptions::default())
}

/// Decode only the luminance of a YCbCr/YCCK JPEG as Gray8 (chroma is
/// entropy-decoded but never transformed). Other color spaces decode fully.
/// For detectors (QR) that only need intensity.
pub fn decode_luma(data: &[u8], limits: &Limits, deadline: &Deadline) -> Result<(Info, Image)> {
    let opts = DecodeOptions {
        luma_only: true,
        ..DecodeOptions::default()
    };
    decode_with(data, limits, deadline, &opts)
}

/// Knobs for embedders whose container overrides JPEG conventions (PDF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeOptions {
    /// See [`decode_luma`].
    pub luma_only: bool,
    /// Skip the YCbCr→RGB transform of 3-component YCbCr data and return
    /// the raw component values (PDF `/ColorTransform 0`).
    pub keep_ycbcr: bool,
    /// Invert CMYK/YCCK output the way Pillow does (Adobe convention).
    /// `false` returns the component values as stored (YCCK still has its
    /// YCC part converted to CMY), which is what PDF `DCTDecode` expects.
    pub invert_cmyk: bool,
    /// EXIF orientation 5..=8 (the ones that swap the axes) applied while
    /// the pixels are written: the image comes back turned, `height` wide,
    /// without a second full-size buffer. Other values leave it as stored.
    pub orientation: u8,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions {
            luma_only: false,
            keep_ycbcr: false,
            invert_cmyk: true,
            orientation: 1,
        }
    }
}

/// [`decode`] with explicit [`DecodeOptions`].
pub fn decode_with(
    data: &[u8],
    limits: &Limits,
    deadline: &Deadline,
    opts: &DecodeOptions,
) -> Result<(Info, Image)> {
    let luma = opts.luma_only;
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(Error::Invalid("not a JPEG file"));
    }
    let mut st = State {
        qt: [None; 4],
        dc: Default::default(),
        ac: Default::default(),
        restart: 0,
        frame: None,
        scans: 0,
        eobrun: 0,
        luma_only: false,
        jfif: false,
        adobe: None,
        opts: *opts,
    };
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
                if st.scans == 0
                    && luma
                    && let Some(f) = &st.frame
                {
                    st.luma_only = matches!(
                        color_transform(&f.info, st.jfif, st.adobe),
                        ColorTransform::YCbCr | ColorTransform::Ycck
                    );
                }
                st.scans += 1;
                if st.scans > MAX_SCANS {
                    return Err(Error::Limit("JPEG scan count"));
                }
                pos = scan(data, body, pos, &mut st, limits, deadline)?;
            }
            0xDC => return Err(Error::Unsupported("JPEG DNL marker")),
            0xE0 if body.starts_with(b"JFIF\0") => st.jfif = true,
            0xE1 if body.starts_with(b"Exif\0") && body.len() > 6 && exif.is_none() => {
                exif = Some(body[6..].to_vec())
            }
            0xE1 if body.starts_with(b"http://ns.adobe.com/xap/1.0/\0") && xmp.is_none() => {
                xmp = Some(body[29..].to_vec())
            }
            0xE2 if body.starts_with(b"ICC_PROFILE\0") && body.len() >= 14 => {
                icc_chunks.push((body[12], body[13], &body[14..]))
            }
            0xEE if body.starts_with(b"Adobe") && body.len() >= 12 => st.adobe = Some(body[11]),
            0xFE => comments.push(body.to_vec()),
            _ => {}
        }
    }
    let mut frame = st.frame.take().ok_or(Error::Invalid("missing SOF"))?;
    if st.scans == 0 {
        return Err(Error::Invalid("no scans"));
    }
    let (jfif, adobe) = (st.jfif, st.adobe);
    let transform = color_transform(&frame.info, jfif, adobe);
    let image = if let Some(stream) = frame.stream.take() {
        let (w, h) = (frame.info.width, frame.info.height);
        let (width, height) = if stream.turn != 0 { (h, w) } else { (w, h) };
        Image {
            width,
            height,
            format: stream.format,
            data: stream.data,
        }
    } else {
        buffered_image(&mut frame, transform, st.luma_only, opts, deadline)?
    };
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
    Ok((info, image))
}

/// The axes-swapping orientation requested, or 0.
fn turn_of(opts: &DecodeOptions) -> u8 {
    if (5..=8).contains(&opts.orientation) {
        opts.orientation
    } else {
        0
    }
}

/// The output transform for `transform` under the caller's options.
fn output_transform(
    transform: ColorTransform,
    luma_only: bool,
    opts: &DecodeOptions,
) -> ColorTransform {
    if luma_only {
        ColorTransform::Gray
    } else if opts.keep_ycbcr && transform == ColorTransform::YCbCr {
        ColorTransform::Rgb
    } else {
        transform
    }
}

/// IDCT and convert a frame whose coefficients were buffered.
fn buffered_image(
    frame: &mut Frame,
    transform: ColorTransform,
    luma_only: bool,
    opts: &DecodeOptions,
    deadline: &Deadline,
) -> Result<Image> {
    let luma_only = luma_only && matches!(transform, ColorTransform::YCbCr | ColorTransform::Ycck);
    let progressive = frame.progressive;
    if luma_only {
        frame.comps.truncate(1);
    }
    {
        for c in &mut frame.comps {
            deadline.check()?;
            let quant = c.quant.ok_or(Error::Invalid("component never scanned"))?;
            c.plane = idct_plane(&c.coefs, progressive, &quant, c.bw, c.bh);
            c.coefs = Vec::new();
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
            band: 0,
        })
        .collect();
    let output = output_transform(transform, luma_only, opts);
    convert(
        &planes,
        output,
        opts.invert_cmyk,
        w,
        h,
        turn_of(opts),
        deadline,
    )
}

photo_core::multiversion! {
    /// Inverse DCT of a whole buffered (progressive) component.
    /// `zigzag`: blocks are in zigzag order (progressive scans).
    fn idct_plane(coefs: &[i16], zigzag: bool, quant: &[u16; 64], bw: usize, bh: usize) -> Vec<u8> = idct_plane_body;
}

#[inline(always)]
fn idct_plane_body(
    coefs: &[i16],
    zigzag: bool,
    quant: &[u16; 64],
    bw: usize,
    bh: usize,
) -> Vec<u8> {
    let stride = bw * 8;
    let mut plane = vec![0u8; bw * bh * 64];
    let mut natural = [0i16; 64];
    for (i, block) in coefs.chunks_exact(64).enumerate() {
        let (by, bx) = (i / bw, i % bw);
        let mut block: &[i16; 64] = block.try_into().expect("64");
        if zigzag {
            for (k, &c) in block.iter().enumerate() {
                natural[ZIGZAG[k]] = c;
            }
            block = &natural;
        }
        idct_islow(block, quant, &mut plane[by * 8 * stride + bx * 8..], stride);
    }
    plane
}

photo_core::multiversion! {
    /// Decode one scan. Returns the offset to continue marker parsing from.
    fn scan(
        data: &[u8],
        header: &[u8],
        start: usize,
        st: &mut State,
        limits: &Limits,
        deadline: &Deadline,
    ) -> Result<usize> = scan_body;
}

#[inline(always)]
fn scan_body(
    data: &[u8],
    header: &[u8],
    start: usize,
    st: &mut State,
    limits: &Limits,
    deadline: &Deadline,
) -> Result<usize> {
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
        let index = frame
            .comps
            .iter()
            .position(|c| c.id == id)
            .ok_or(Error::Invalid("SOS component"))?;
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
    if frame.progressive
        && (ss > se || se > 63 || (ss == 0 && se != 0) || (ss > 0 && ns != 1) || al > 13 || ah > 13)
    {
        return Err(Error::Invalid("progressive scan parameters"));
    }
    if ns > 1
        && sc
            .iter()
            .map(|s| frame.comps[s.index].h * frame.comps[s.index].v)
            .sum::<usize>()
            > 10
    {
        return Err(Error::Invalid("too many blocks per MCU"));
    }
    if frame.buffered.is_none() {
        let direct = !frame.progressive && ns == frame.comps.len();
        let output = direct.then(|| {
            let transform = color_transform(&frame.info, st.jfif, st.adobe);
            let luma = st.luma_only;
            (
                output_transform(transform, luma, &st.opts),
                st.opts.invert_cmyk,
            )
        });
        frame.allocate(!direct, output, turn_of(&st.opts), limits)?;
    } else if frame.buffered == Some(false) {
        return Err(Error::Invalid("extra scan in single-scan sequential JPEG"));
    }
    let buffered = frame.buffered == Some(true);
    let luma_only = st.luma_only;
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
                        let (bx, by) = if ns == 1 {
                            (ux, uy)
                        } else {
                            (ux * c.h + h, uy * c.v + v)
                        };
                        if buffered {
                            let i = (by * c.bw + bx) * 64;
                            let coefs: &mut [i16; 64] =
                                (&mut c.coefs[i..i + 64]).try_into().expect("64");
                            if frame.progressive {
                                if ss == 0 {
                                    if ah == 0 {
                                        let t = st.dc[s.dc].as_ref().expect("checked");
                                        let s0 = u32::from(r.decode(t)?);
                                        let diff = r.receive_extend(s0)?;
                                        c.dc_pred = c.dc_pred.wrapping_add(diff);
                                        coefs[0] = (c.dc_pred << al) as i16;
                                    } else if r.bit() == 1 {
                                        coefs[0] |= (1 << al) as i16;
                                    }
                                } else if ah == 0 {
                                    ac_first(
                                        &mut r,
                                        st.ac[s.ac].as_ref().expect("checked"),
                                        ss,
                                        se,
                                        al,
                                        &mut st.eobrun,
                                        coefs,
                                    )?;
                                } else {
                                    ac_refine(
                                        &mut r,
                                        st.ac[s.ac].as_ref().expect("checked"),
                                        ss,
                                        se,
                                        al,
                                        &mut st.eobrun,
                                        coefs,
                                    )?;
                                }
                            } else {
                                sequential(
                                    &mut r,
                                    st.dc[s.dc].as_ref().expect("checked"),
                                    st.ac[s.ac].as_ref().expect("checked"),
                                    &mut c.dc_pred,
                                    coefs,
                                )?;
                            }
                        } else {
                            sequential(
                                &mut r,
                                st.dc[s.dc].as_ref().expect("checked"),
                                st.ac[s.ac].as_ref().expect("checked"),
                                &mut c.dc_pred,
                                &mut block,
                            )?;
                            if luma_only && s.index != 0 {
                                continue;
                            }
                            let q = c.quant.as_ref().expect("latched");
                            let stream = frame.stream.as_mut().expect("direct frame");
                            let p = &mut stream.planes[s.index];
                            let at = p.offset(by * 8) + bx * 8;
                            idct_islow(&block, q, &mut p.data[at..], p.stride);
                        }
                    }
                }
            }
            if r.overrun() {
                return Err(Error::Truncated);
            }
        }
        if !buffered && uy > 0 {
            let (w, h) = (frame.info.width as usize, frame.info.height as usize);
            frame
                .stream
                .as_mut()
                .expect("direct frame")
                .emit(uy - 1, w, h);
        }
    }
    if !buffered && units_y > 0 {
        let (w, h) = (frame.info.width as usize, frame.info.height as usize);
        frame
            .stream
            .as_mut()
            .expect("direct frame")
            .emit(units_y - 1, w, h);
    }
    let (m, at) = match r.finish_segment() {
        Ok(v) => v,
        Err(Error::Truncated) => return Ok(data.len()),
        Err(e) => return Err(e),
    };
    let _ = m;
    Ok(at)
}

#[inline(always)]
fn sequential(
    r: &mut BitReader,
    dc: &HuffTable,
    ac: &HuffTable,
    pred: &mut i32,
    block: &mut [i16; 64],
) -> Result<()> {
    *block = [0; 64];
    let s = u32::from(r.decode(dc)?);
    let diff = r.receive_extend(s)?;
    *pred = pred.wrapping_add(diff);
    block[0] = *pred as i16;
    let mut k = 1;
    while k < 64 {
        let fac = ac.fast_ac[r.peek_fast()];
        if fac != 0 {
            r.skip((fac & 0xFF) as u32);
            k += ((fac >> 8) & 15) as usize;
            block[ZIGZAG[k]] = (fac >> 16) as i16;
            k += 1;
            continue;
        }
        let rs = r.decode(ac)?;
        let (run, s) = (usize::from(rs >> 4), u32::from(rs & 15));
        if s != 0 {
            k += run;
            let v = r.receive_extend(s)?;
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

// Progressive scans keep each block's coefficients in zigzag order, so a
// spectral band is a contiguous slice and the refinement passes can find
// nonzero coefficients with bit masks; `idct_plane` restores natural order.
// Positions past 63 (corrupt run lengths) land on 63, as libjpeg's padded
// natural-order table does.

#[inline(always)]
fn ac_first(
    r: &mut BitReader,
    t: &HuffTable,
    ss: usize,
    se: usize,
    al: u32,
    eobrun: &mut u32,
    zz: &mut [i16; 64],
) -> Result<()> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = ss;
    while k <= se {
        let fac = t.fast_ac[r.peek_fast()];
        if fac != 0 {
            r.skip((fac & 0xFF) as u32);
            k += ((fac >> 8) & 15) as usize;
            zz[k.min(63)] = ((fac >> 16) << al) as i16;
            k += 1;
            continue;
        }
        let rs = r.decode(t)?;
        let (run, s) = (u32::from(rs >> 4), u32::from(rs & 15));
        if s != 0 {
            k += run as usize;
            let v = r.receive_extend(s)?;
            zz[k.min(63)] = (v << al) as i16;
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

/// Bits `a..=b` set (`a <= b <= 63`).
#[inline(always)]
fn band(a: usize, b: usize) -> u64 {
    (u64::MAX << a) & (u64::MAX >> (63 - b))
}

#[inline(always)]
fn ac_refine(
    r: &mut BitReader,
    t: &HuffTable,
    ss: usize,
    se: usize,
    al: u32,
    eobrun: &mut u32,
    zz: &mut [i16; 64],
) -> Result<()> {
    let p1: i16 = 1 << al;
    let m1: i16 = (-1i32 << al) as i16;
    // Correction bits for the nonzero coefficients in `mask`, in order.
    let refine = |r: &mut BitReader, zz: &mut [i16; 64], mut mask: u64| {
        while mask != 0 {
            let c = &mut zz[mask.trailing_zeros() as usize];
            mask &= mask - 1;
            if r.bit() == 1 && (*c & p1) == 0 {
                *c = if *c >= 0 {
                    c.wrapping_add(p1)
                } else {
                    c.wrapping_add(m1)
                };
            }
        }
    };
    // Refinement never makes a coefficient zero, so this mask only gains
    // the newly set ones below.
    let mut nz = zz
        .iter()
        .enumerate()
        .fold(0u64, |m, (i, &c)| m | (u64::from(c != 0) << i));
    let mut k = ss;
    if *eobrun == 0 {
        while k <= se {
            let rs = r.decode(t)?;
            let (run, s) = (u32::from(rs >> 4), rs & 15);
            let mut value = 0i16;
            if s != 0 {
                value = if r.bit() == 1 { p1 } else { m1 };
            } else if run != 15 {
                *eobrun = 1 << run;
                if run > 0 {
                    *eobrun += r.bits(run);
                }
                break;
            }
            // Skip `run` zero coefficients, refining the nonzero ones
            // passed, and stop on the next zero (or after `se`).
            let mut zeros = !nz & band(k, se);
            for _ in 0..run {
                zeros &= zeros.wrapping_sub(1);
            }
            if zeros == 0 {
                refine(r, zz, nz & band(k, se));
                k = se + 1;
            } else {
                let z = zeros.trailing_zeros() as usize;
                refine(r, zz, nz & band(k, se) & ((1u64 << z) - 1));
                k = z;
            }
            if value != 0 {
                let i = k.min(63);
                zz[i] = value;
                nz |= 1 << i;
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        if k <= se {
            refine(r, zz, nz & band(k, se));
        }
        *eobrun -= 1;
    }
    Ok(())
}
