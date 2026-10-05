//! Upsampling (`jdsample.c`, fancy where libjpeg-turbo uses it) and color
//! conversion (`jdcolor.c` fixed-point tables), row by row.

use crate::markers::ColorTransform;
use photo_core::{Deadline, Error, Image, PixelFormat, Result};

/// One decoded component plane (padded to whole blocks).
pub(crate) struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
    /// Real (downsampled) size: ceil(image * factor / max_factor).
    pub width: usize,
    pub height: usize,
    pub h_ratio: usize,
    pub v_ratio: usize,
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

impl Plane {
    fn row(&self, y: usize) -> &[u8] {
        &self.data[y * self.stride..y * self.stride + self.stride]
    }

    /// Upsampled samples of output row `y` into `out` (len >= image width).
    fn upsample(&self, m: Method, y: usize, out: &mut [u8]) {
        let w = out.len();
        match m {
            Method::Full => out.copy_from_slice(&self.row(y)[..w]),
            Method::Box => {
                let src = self.row(y / self.v_ratio);
                for (x, o) in out.iter_mut().enumerate() {
                    *o = src[x / self.h_ratio];
                }
            }
            Method::H2V1Fancy => h2v1(self.row(y), self.width, out),
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
                h2v2(self.row(iy), self.row(near), self.width, out);
            }
        }
    }

    /// Row used as the "next nearest" context and the h1v2 rounding bias.
    fn neighbor(&self, y: usize, iy: usize) -> (usize, u32) {
        if y % 2 == 0 {
            (iy.saturating_sub(1), 1)
        } else {
            ((iy + 1).min(self.height - 1), 2)
        }
    }
}

fn h2v1(inp: &[u8], dw: usize, out: &mut [u8]) {
    let w = out.len();
    let mut tmp = [0u8; 2];
    let mut put = |i: usize, v: u32| {
        if i < w {
            out[i] = v as u8;
        } else if i - w < 2 {
            tmp[i - w] = v as u8;
        }
    };
    let p = |i: usize| u32::from(inp[i]);
    put(0, p(0));
    put(1, (p(0) * 3 + p(1) + 2) >> 2);
    for c in 1..dw - 1 {
        let v = p(c) * 3;
        put(2 * c, (v + p(c - 1) + 1) >> 2);
        put(2 * c + 1, (v + p(c + 1) + 2) >> 2);
    }
    let l = dw - 1;
    put(2 * l, (p(l) * 3 + p(l - 1) + 1) >> 2);
    put(2 * l + 1, p(l));
}

fn h2v2(in0: &[u8], in1: &[u8], dw: usize, out: &mut [u8]) {
    let w = out.len();
    let mut put = |i: usize, v: u32| {
        if i < w {
            out[i] = v as u8;
        }
    };
    let sum = |c: usize| u32::from(in0[c]) * 3 + u32::from(in1[c]);
    let mut this = sum(0);
    let mut next = sum(1);
    put(0, (this * 4 + 8) >> 4);
    put(1, (this * 3 + next + 7) >> 4);
    let mut last = this;
    this = next;
    for c in 1..dw - 1 {
        next = sum(c + 1);
        put(2 * c, (this * 3 + last + 8) >> 4);
        put(2 * c + 1, (this * 3 + next + 7) >> 4);
        last = this;
        this = next;
    }
    let l = dw - 1;
    put(2 * l, (this * 3 + last + 8) >> 4);
    put(2 * l + 1, (this * 4 + 7) >> 4);
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
        let mut t = Tables { cr_r: [0; 256], cb_b: [0; 256], cr_g: [0; 256], cb_g: [0; 256] };
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

/// Convert decoded planes into an interleaved image. CMYK output is
/// inverted as Pillow does for all CMYK JPEGs ("CMYK;I", Adobe convention).
pub(crate) fn convert(
    planes: &[Plane],
    transform: ColorTransform,
    width: usize,
    height: usize,
    deadline: &Deadline,
) -> Result<Image> {
    let format = match transform {
        ColorTransform::Gray => PixelFormat::Gray8,
        ColorTransform::YCbCr | ColorTransform::Rgb => PixelFormat::Rgb8,
        ColorTransform::Cmyk | ColorTransform::Ycck => PixelFormat::Cmyk8,
    };
    let ch = format.channels();
    if planes.len() != ch {
        return Err(Error::Unsupported("JPEG component count"));
    }
    let methods: Vec<Method> = planes.iter().map(method).collect();
    let mut data = vec![0u8; width * height * ch];
    let mut rows: Vec<Vec<u8>> = (0..ch).map(|_| vec![0u8; width]).collect();
    let t = tables();
    for (y, out) in data.chunks_exact_mut(width * ch).enumerate() {
        if y % 64 == 0 {
            deadline.check()?;
        }
        for (c, p) in planes.iter().enumerate() {
            p.upsample(methods[c], y, &mut rows[c]);
        }
        match transform {
            ColorTransform::Gray => out.copy_from_slice(&rows[0]),
            ColorTransform::Rgb => {
                for x in 0..width {
                    out[3 * x] = rows[0][x];
                    out[3 * x + 1] = rows[1][x];
                    out[3 * x + 2] = rows[2][x];
                }
            }
            ColorTransform::YCbCr => {
                let (ys, cbs, crs) = (&rows[0], &rows[1], &rows[2]);
                for x in 0..width {
                    let yy = i32::from(ys[x]);
                    let cb = cbs[x] as usize;
                    let cr = crs[x] as usize;
                    out[3 * x] = clamp(yy + t.cr_r[cr]);
                    out[3 * x + 1] = clamp(yy + ((t.cb_g[cb] + t.cr_g[cr]) >> 16));
                    out[3 * x + 2] = clamp(yy + t.cb_b[cb]);
                }
            }
            ColorTransform::Cmyk => {
                for x in 0..width {
                    for c in 0..4 {
                        out[4 * x + c] = 255 - rows[c][x];
                    }
                }
            }
            ColorTransform::Ycck => {
                for x in 0..width {
                    let yy = i32::from(rows[0][x]);
                    let cb = rows[1][x] as usize;
                    let cr = rows[2][x] as usize;
                    out[4 * x] = 255 - clamp(255 - (yy + t.cr_r[cr]));
                    out[4 * x + 1] = 255 - clamp(255 - (yy + ((t.cb_g[cb] + t.cr_g[cr]) >> 16)));
                    out[4 * x + 2] = 255 - clamp(255 - (yy + t.cb_b[cb]));
                    out[4 * x + 3] = 255 - rows[3][x];
                }
            }
        }
    }
    Ok(Image { width: width as u32, height: height as u32, format, data })
}
