//! Upsampling (`jdsample.c`, fancy where libjpeg-turbo uses it) and color
//! conversion (`jdcolor.c` fixed-point tables), row by row.

use crate::markers::ColorTransform;
use photo_core::{Deadline, Error, Image, PixelFormat, Result};

/// One decoded component plane (padded to whole blocks), either whole or a
/// ring of three MCU-row bands (the band being converted plus its upper and
/// lower neighbours, which fancy upsampling reads).
pub(crate) struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
    /// Real (downsampled) size: ceil(image * factor / max_factor).
    pub width: usize,
    pub height: usize,
    pub h_ratio: usize,
    pub v_ratio: usize,
    /// Sample rows per ring band; 0 for a whole plane.
    pub band: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Full,
    H2V1Fancy,
    H1V2Fancy,
    H2V2Fancy,
    Box,
}

fn method(p: &Plane) -> Method {
    match (p.h_ratio, p.v_ratio) {
        (1, 1) => Method::Full,
        (2, 1) if p.width > 2 => Method::H2V1Fancy,
        (1, 2) => Method::H1V2Fancy,
        (2, 2) if p.width > 2 => Method::H2V2Fancy,
        _ => Method::Box,
    }
}

/// Bands kept by a streaming plane.
pub(crate) const RING: usize = 3;

impl Plane {
    /// Offset of sample row `y` in `data`.
    #[inline(always)]
    pub fn offset(&self, y: usize) -> usize {
        let r = match y.checked_div(self.band) {
            None => y,
            Some(band) => band % RING * self.band + y % self.band,
        };
        r * self.stride
    }

    fn row(&self, y: usize) -> &[u8] {
        let o = self.offset(y);
        &self.data[o..o + self.stride]
    }

    /// Upsampled samples of output row `y` into `out` (len >= image width).
    #[inline(always)]
    fn upsample(
        &self,
        m: Method,
        y: usize,
        out: &mut [u8],
        tmp: &mut Vec<u8>,
        sums: &mut Vec<u32>,
    ) {
        let w = out.len();
        match m {
            Method::Full => out.copy_from_slice(&self.row(y)[..w]),
            Method::Box => {
                let src = self.row(y / self.v_ratio);
                for (x, o) in out.iter_mut().enumerate() {
                    *o = src[x / self.h_ratio];
                }
            }
            Method::H2V1Fancy => h2v1(self.row(y), self.width, out, tmp),
            Method::H1V2Fancy => {
                let iy = y / 2;
                let (near, bias) = self.neighbor(y, iy);
                let a = self.row(iy);
                let b = self.row(near);
                for (x, o) in out.iter_mut().enumerate() {
                    *o = ((u32::from(a[x]) * 3 + u32::from(b[x]) + bias) >> 2) as u8;
                }
            }
            Method::H2V2Fancy => {
                let iy = y / 2;
                let (near, _) = self.neighbor(y, iy);
                h2v2(self.row(iy), self.row(near), self.width, out, tmp, sums);
            }
        }
    }

    /// Row used as the "next nearest" context and the h1v2 rounding bias.
    fn neighbor(&self, y: usize, iy: usize) -> (usize, u32) {
        if y.is_multiple_of(2) {
            (iy.saturating_sub(1), 1)
        } else {
            ((iy + 1).min(self.height - 1), 2)
        }
    }
}

#[inline(always)]
fn h2v1(inp: &[u8], dw: usize, out: &mut [u8], tmp: &mut Vec<u8>) {
    tmp.resize(2 * dw, 0);
    let t = &mut tmp[..2 * dw];
    let p = &inp[..dw];
    t[0] = p[0];
    t[1] = ((u32::from(p[0]) * 3 + u32::from(p[1]) + 2) >> 2) as u8;
    for c in 1..dw - 1 {
        let v = u32::from(p[c]) * 3;
        t[2 * c] = ((v + u32::from(p[c - 1]) + 1) >> 2) as u8;
        t[2 * c + 1] = ((v + u32::from(p[c + 1]) + 2) >> 2) as u8;
    }
    let l = dw - 1;
    t[2 * l] = ((u32::from(p[l]) * 3 + u32::from(p[l - 1]) + 1) >> 2) as u8;
    t[2 * l + 1] = p[l];
    let w = out.len();
    out.copy_from_slice(&t[..w]);
}

#[inline(always)]
fn h2v2(in0: &[u8], in1: &[u8], dw: usize, out: &mut [u8], tmp: &mut Vec<u8>, sums: &mut Vec<u32>) {
    sums.resize(dw, 0);
    for (s, (&a, &b)) in sums.iter_mut().zip(in0[..dw].iter().zip(&in1[..dw])) {
        *s = u32::from(a) * 3 + u32::from(b);
    }
    tmp.resize(2 * dw, 0);
    let t = &mut tmp[..2 * dw];
    let s = &sums[..dw];
    t[0] = ((s[0] * 4 + 8) >> 4) as u8;
    t[1] = ((s[0] * 3 + s[1] + 7) >> 4) as u8;
    for c in 1..dw - 1 {
        t[2 * c] = ((s[c] * 3 + s[c - 1] + 8) >> 4) as u8;
        t[2 * c + 1] = ((s[c] * 3 + s[c + 1] + 7) >> 4) as u8;
    }
    let l = dw - 1;
    t[2 * l] = ((s[l] * 3 + s[l - 1] + 8) >> 4) as u8;
    t[2 * l + 1] = ((s[l] * 4 + 7) >> 4) as u8;
    let w = out.len();
    out.copy_from_slice(&t[..w]);
}

struct Tables {
    cr_r: [i32; 256],
    cb_b: [i32; 256],
    cr_g: [i32; 256],
    cb_g: [i32; 256],
}

fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        const SCALEBITS: i32 = 16;
        const ONE_HALF: i64 = 1 << (SCALEBITS - 1);
        let fix = |x: f64| (x * f64::from(1u32 << SCALEBITS) + 0.5) as i64;
        let mut t = Tables {
            cr_r: [0; 256],
            cb_b: [0; 256],
            cr_g: [0; 256],
            cb_g: [0; 256],
        };
        for i in 0..256 {
            let x = i as i64 - 128;
            t.cr_r[i] = ((fix(1.40200) * x + ONE_HALF) >> SCALEBITS) as i32;
            t.cb_b[i] = ((fix(1.77200) * x + ONE_HALF) >> SCALEBITS) as i32;
            t.cr_g[i] = (-fix(0.71414) * x) as i32;
            t.cb_g[i] = (-fix(0.34414) * x + ONE_HALF) as i32;
        }
        t
    })
}

#[inline(always)]
fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

pub(crate) fn output_format(transform: ColorTransform) -> PixelFormat {
    match transform {
        ColorTransform::Gray => PixelFormat::Gray8,
        ColorTransform::YCbCr | ColorTransform::Rgb => PixelFormat::Rgb8,
        ColorTransform::Cmyk | ColorTransform::Ycck => PixelFormat::Cmyk8,
    }
}

/// Upsampling and color conversion of output rows, any number at a time.
pub(crate) struct Converter {
    transform: ColorTransform,
    /// Pillow's "CMYK;I": invert (255). PDF: as stored (0).
    inv: u8,
    width: usize,
    methods: Vec<Method>,
    rows: Vec<Vec<u8>>,
    tmp: Vec<u8>,
    sums: Vec<u32>,
}

impl Converter {
    /// CMYK output is inverted as Pillow does for all CMYK JPEGs
    /// ("CMYK;I", Adobe convention) unless `invert_cmyk` is false.
    pub fn new(
        planes: &[Plane],
        transform: ColorTransform,
        invert_cmyk: bool,
        width: usize,
    ) -> Result<Converter> {
        let ch = output_format(transform).channels();
        if planes.len() != ch {
            return Err(Error::Unsupported("JPEG component count"));
        }
        Ok(Converter {
            transform,
            inv: if invert_cmyk { 255 } else { 0 },
            width,
            methods: planes.iter().map(method).collect(),
            rows: (0..ch).map(|_| vec![0u8; width]).collect(),
            tmp: Vec::new(),
            sums: Vec::new(),
        })
    }

    /// Convert output rows starting at `y0` into `out` (whole rows).
    pub fn rows(&mut self, planes: &[Plane], y0: usize, out: &mut [u8]) {
        convert_rows(self, planes, y0, out)
    }
}

/// Convert whole planes into an interleaved image.
pub(crate) fn convert(
    planes: &[Plane],
    transform: ColorTransform,
    invert_cmyk: bool,
    width: usize,
    height: usize,
    turn: u8,
    deadline: &Deadline,
) -> Result<Image> {
    let format = output_format(transform);
    let ch = format.channels();
    let mut conv = Converter::new(planes, transform, invert_cmyk, width)?;
    let mut data = vec![0u8; width * height * ch];
    if turn != 0 {
        // Convert 64 rows at a time and place them turned (`crate::turn`).
        let mut band = vec![0u8; 64 * width * ch];
        for y0 in (0..height).step_by(64) {
            deadline.check()?;
            let band = &mut band[..(height - y0).min(64) * width * ch];
            conv.rows(planes, y0, band);
            crate::turn::place(band, y0, width, height, ch, turn, &mut data);
        }
        return Ok(Image {
            width: height as u32,
            height: width as u32,
            format,
            data,
        });
    }
    for (i, chunk) in data.chunks_mut(64 * width * ch).enumerate() {
        deadline.check()?;
        conv.rows(planes, i * 64, chunk);
    }
    Ok(Image {
        width: width as u32,
        height: height as u32,
        format,
        data,
    })
}

photo_core::multiversion! {
    fn convert_rows(conv: &mut Converter, planes: &[Plane], y0: usize, out: &mut [u8]) -> () = convert_rows_body;
}

#[inline(always)]
fn convert_rows_body(conv: &mut Converter, planes: &[Plane], y0: usize, out: &mut [u8]) {
    let width = conv.width;
    let ch = conv.rows.len();
    let t = tables();
    let inv = conv.inv;
    let Converter {
        transform,
        methods,
        rows,
        tmp,
        sums,
        ..
    } = conv;
    for (i, out) in out.chunks_exact_mut(width * ch).enumerate() {
        let y = y0 + i;
        for (c, p) in planes.iter().enumerate() {
            p.upsample(methods[c], y, &mut rows[c], tmp, sums);
        }
        match *transform {
            ColorTransform::Gray => out.copy_from_slice(&rows[0]),
            ColorTransform::Rgb => {
                for x in 0..width {
                    out[3 * x] = rows[0][x];
                    out[3 * x + 1] = rows[1][x];
                    out[3 * x + 2] = rows[2][x];
                }
            }
            ColorTransform::YCbCr => ycc_row(&rows[0], &rows[1], &rows[2], out),
            ColorTransform::Cmyk => {
                for x in 0..width {
                    for c in 0..4 {
                        out[4 * x + c] = inv ^ rows[c][x];
                    }
                }
            }
            ColorTransform::Ycck => {
                for x in 0..width {
                    let yy = i32::from(rows[0][x]);
                    let cb = rows[1][x] as usize;
                    let cr = rows[2][x] as usize;
                    out[4 * x] = inv ^ clamp(255 - (yy + t.cr_r[cr]));
                    out[4 * x + 1] = inv ^ clamp(255 - (yy + ((t.cb_g[cb] + t.cr_g[cr]) >> 16)));
                    out[4 * x + 2] = inv ^ clamp(255 - (yy + t.cb_b[cb]));
                    out[4 * x + 3] = inv ^ rows[3][x];
                }
            }
        }
    }
}

/// jdcolor.c `ycc_rgb_convert` with the table entries computed inline
/// (identical integers; branch-free so it vectorizes).
#[inline(always)]
fn ycc_row(ys: &[u8], cbs: &[u8], crs: &[u8], out: &mut [u8]) {
    const CR_R: i32 = 91881; // FIX(1.40200)
    const CB_B: i32 = 116130; // FIX(1.77200)
    const CR_G: i32 = 46802; // FIX(0.71414)
    const CB_G: i32 = 22554; // FIX(0.34414)
    const HALF: i32 = 1 << 15;
    for (((o, &y), &cb), &cr) in out.chunks_exact_mut(3).zip(ys).zip(cbs).zip(crs) {
        let (y, cb, cr) = (i32::from(y), i32::from(cb) - 128, i32::from(cr) - 128);
        let r = y + ((CR_R * cr + HALF) >> 16);
        let g = y + ((-CB_G * cb + HALF - CR_G * cr) >> 16);
        let b = y + ((CB_B * cb + HALF) >> 16);
        o[0] = r.clamp(0, 255) as u8;
        o[1] = g.clamp(0, 255) as u8;
        o[2] = b.clamp(0, 255) as u8;
    }
}
