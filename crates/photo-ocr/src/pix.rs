//! The Leptonica operations Tesseract applies to line images, ported with
//! the same integer/float arithmetic (`pixConvertRGBToLuminance`,
//! `pixScale` for 8 bpp, `pixUnsharpMaskingGray2D`).

/// 8-bit grey image, row-major, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Gray {
    pub fn new(width: usize, height: usize) -> Gray {
        Gray {
            width,
            height,
            data: vec![0; width * height],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.width + x]
    }

    #[inline]
    fn set(&mut self, x: usize, y: usize, v: u8) {
        self.data[y * self.width + x] = v;
    }

    /// `pixClipRectangle` (the box is already clipped to the image).
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Gray {
        let mut out = Gray::new(w, h);
        for r in 0..h {
            let s = (y + r) * self.width + x;
            out.data[r * w..(r + 1) * w].copy_from_slice(&self.data[s..s + w]);
        }
        out
    }

    pub fn invert(&mut self) {
        self.data.iter_mut().for_each(|v| *v = !*v);
    }
}

/// `pixConvertRGBToLuminance` on one pixel (weights 0.3/0.5/0.2 as float,
/// the +0.5 in double, truncation).
#[inline]
pub fn luminance(r: u8, g: u8, b: u8) -> u8 {
    let sum: f32 = 0.3f32 * f32::from(r) + 0.5f32 * f32::from(g) + 0.2f32 * f32::from(b);
    (f64::from(sum) + 0.5) as i32 as u8
}

/// `pixScale` for an 8 bpp image.
pub fn scale(src: &Gray, scalex: f32, scaley: f32) -> Gray {
    if scalex == 1.0 && scaley == 1.0 {
        return src.clone();
    }
    // Leptonica compares the float factors with double constants: a
    // factor of exactly 0.2f is "> 0.2" (0.2f rounds up), 0.7f is "< 0.7".
    let maxscale = f64::from(scalex.max(scaley));
    let minscale = f64::from(scalex.min(scaley));
    let (sharpfract, sharpwidth) = if maxscale < 0.7 {
        (0.2f32, 1)
    } else {
        (0.4f32, 2)
    };
    if maxscale < 0.7 {
        let low = if minscale < 0.02 {
            scale_smooth(src, scalex, scaley)
        } else {
            scale_area_map(src, scalex, scaley)
        };
        if maxscale > 0.2 {
            unsharp_mask(&low, sharpwidth, sharpfract)
        } else {
            low
        }
    } else {
        let li = scale_gray_li(src, scalex, scaley);
        if maxscale < 1.4 {
            unsharp_mask(&li, sharpwidth, sharpfract)
        } else {
            li
        }
    }
}

fn dest_size(scale: f32, s: usize) -> usize {
    (scale * s as f32 + 0.5) as i32 as usize
}

/// `pixScaleGrayLI` (with the 2x special case; `scaleGrayLILow` otherwise).
fn scale_gray_li(src: &Gray, scalex: f32, scaley: f32) -> Gray {
    if scalex == 2.0 && scaley == 2.0 {
        return scale_gray_2x_li(src);
    }
    if scalex == 4.0 && scaley == 4.0 {
        return scale_gray_4x_li(src);
    }
    let (ws, hs) = (src.width, src.height);
    let (wd, hd) = (dest_size(scalex, ws), dest_size(scaley, hs));
    let mut d = Gray::new(wd, hd);
    if wd == 0 || hd == 0 {
        return d;
    }
    let scx = (16.0f64 * ws as f32 as f64 / wd as f32 as f64) as f32;
    let scy = (16.0f64 * hs as f32 as f64 / hd as f32 as f64) as f32;
    let wm2 = ws as i32 - 2;
    let hm2 = hs as i32 - 2;
    for i in 0..hd {
        let ypm = (scy * i as f32) as i32;
        let yp = ypm >> 4;
        let yf = ypm & 0x0f;
        for j in 0..wd {
            let xpm = (scx * j as f32) as i32;
            let xp = xpm >> 4;
            let xf = xpm & 0x0f;
            let px = |x: i32, y: i32| i32::from(src.get(x as usize, y as usize));
            let v00 = px(xp, yp);
            let (v01, v10, v11);
            if xp > wm2 || yp > hm2 {
                if yp > hm2 && xp <= wm2 {
                    v01 = v00;
                    v10 = px(xp + 1, yp);
                    v11 = v10;
                } else if xp > wm2 && yp <= hm2 {
                    v01 = px(xp, yp + 1);
                    v10 = v00;
                    v11 = v01;
                } else {
                    v01 = v00;
                    v10 = v00;
                    v11 = v00;
                }
            } else {
                v10 = px(xp + 1, yp);
                v01 = px(xp, yp + 1);
                v11 = px(xp + 1, yp + 1);
            }
            let a = (16 - xf) * (16 - yf) * v00;
            let b = xf * (16 - yf) * v10;
            let c = (16 - xf) * yf * v01;
            let e = xf * yf * v11;
            d.set(j, i, ((a + c + b + e + 128) / 256) as u8);
        }
    }
    d
}

/// `scaleGray4xLILow`.
fn scale_gray_4x_li(src: &Gray) -> Gray {
    let (ws, hs) = (src.width, src.height);
    let mut d = Gray::new(4 * ws, 4 * hs);
    if ws == 0 || hs == 0 {
        return d;
    }
    let wsm = ws - 1;
    for i in 0..hs {
        let last = i == hs - 1;
        let y = 4 * i;
        let mut put = |x: usize, r: usize, v: u32| d.set(x, y + r, v as u8);
        for j in 0..wsm {
            let jd = 4 * j;
            let s1 = u32::from(src.get(j, i));
            let s2 = u32::from(src.get(j + 1, i));
            let (s1t, s2t) = (3 * s1, 3 * s2);
            if last {
                for r in 0..4 {
                    put(jd, r, s1);
                    put(jd + 1, r, (s1t + s2) / 4);
                    put(jd + 2, r, (s1 + s2) / 2);
                    put(jd + 3, r, (s1 + s2t) / 4);
                }
            } else {
                let s3 = u32::from(src.get(j, i + 1));
                let s4 = u32::from(src.get(j + 1, i + 1));
                let (s3t, s4t) = (3 * s3, 3 * s4);
                put(jd, 0, s1);
                put(jd + 1, 0, (s1t + s2) / 4);
                put(jd + 2, 0, (s1 + s2) / 2);
                put(jd + 3, 0, (s1 + s2t) / 4);
                put(jd, 1, (s1t + s3) / 4);
                put(jd + 1, 1, (9 * s1 + s2t + s3t + s4) / 16);
                put(jd + 2, 1, (s1t + s2t + s3 + s4) / 8);
                put(jd + 3, 1, (s1t + 9 * s2 + s3 + s4t) / 16);
                put(jd, 2, (s1 + s3) / 2);
                put(jd + 1, 2, (s1t + s2 + s3t + s4) / 8);
                put(jd + 2, 2, (s1 + s2 + s3 + s4) / 4);
                put(jd + 3, 2, (s1 + s2t + s3 + s4t) / 8);
                put(jd, 3, (s1 + s3t) / 4);
                put(jd + 1, 3, (s1t + s2 + 9 * s3 + s4t) / 16);
                put(jd + 2, 3, (s1 + s2 + s3t + s4t) / 8);
                put(jd + 3, 3, (s1 + s2t + s3t + 9 * s4) / 16);
            }
        }
        let w4 = 4 * wsm;
        let s1 = u32::from(src.get(wsm, i));
        let rows: [u32; 4] = if last {
            [s1; 4]
        } else {
            let s3 = u32::from(src.get(wsm, i + 1));
            [s1, (3 * s1 + s3) / 4, (s1 + s3) / 2, (s1 + 3 * s3) / 4]
        };
        for (r, v) in rows.into_iter().enumerate() {
            for k in 0..4 {
                put(w4 + k, r, v);
            }
        }
    }
    d
}

/// `scaleGray2xLILow` (per pixel form of the unrolled loop).
fn scale_gray_2x_li(src: &Gray) -> Gray {
    let (ws, hs) = (src.width, src.height);
    let mut d = Gray::new(2 * ws, 2 * hs);
    if ws == 0 || hs == 0 {
        return d;
    }
    let wsm = ws - 1;
    for i in 0..hs {
        let last = i == hs - 1;
        let (yd, ydp) = (2 * i, 2 * i + 1);
        for j in 0..wsm {
            let s1 = u32::from(src.get(j, i));
            let s2 = u32::from(src.get(j + 1, i));
            if last {
                d.set(2 * j, yd, s1 as u8);
                d.set(2 * j, ydp, s1 as u8);
                d.set(2 * j + 1, yd, ((s1 + s2) / 2) as u8);
                d.set(2 * j + 1, ydp, ((s1 + s2) / 2) as u8);
            } else {
                let s3 = u32::from(src.get(j, i + 1));
                let s4 = u32::from(src.get(j + 1, i + 1));
                d.set(2 * j, yd, s1 as u8);
                d.set(2 * j + 1, yd, ((s1 + s2) / 2) as u8);
                d.set(2 * j, ydp, ((s1 + s3) / 2) as u8);
                d.set(2 * j + 1, ydp, ((s1 + s2 + s3 + s4) / 4) as u8);
            }
        }
        let s1 = u32::from(src.get(wsm, i));
        if last {
            for (x, y) in [
                (2 * wsm, yd),
                (2 * wsm + 1, yd),
                (2 * wsm, ydp),
                (2 * wsm + 1, ydp),
            ] {
                d.set(x, y, s1 as u8);
            }
        } else {
            let s3 = u32::from(src.get(wsm, i + 1));
            d.set(2 * wsm, yd, s1 as u8);
            d.set(2 * wsm + 1, yd, s1 as u8);
            d.set(2 * wsm, ydp, ((s1 + s3) / 2) as u8);
            d.set(2 * wsm + 1, ydp, ((s1 + s3) / 2) as u8);
        }
    }
    d
}

/// `pixScaleAreaMap` for 8 bpp.
fn scale_area_map(src: &Gray, scalex: f32, scaley: f32) -> Gray {
    if scalex == 0.5 && scaley == 0.5 {
        return area_map2(src);
    }
    if scalex == 0.25 && scaley == 0.25 {
        return area_map2(&area_map2(src));
    }
    if scalex == 0.125 && scaley == 0.125 {
        return area_map2(&area_map2(&area_map2(src)));
    }
    if scalex == 0.0625 && scaley == 0.0625 {
        return area_map2(&area_map2(&area_map2(&area_map2(src))));
    }
    let (ws, hs) = (src.width, src.height);
    let (wd, hd) = (dest_size(scalex, ws), dest_size(scaley, hs));
    let mut d = Gray::new(wd, hd);
    if wd == 0 || hd == 0 {
        return d;
    }
    let scx = (16.0f64 * ws as f32 as f64 / wd as f32 as f64) as f32;
    let scy = (16.0f64 * hs as f32 as f64 / hd as f32 as f64) as f32;
    let wm2 = ws as i32 - 2;
    let hm2 = hs as i32 - 2;
    let px = |x: i32, y: i32| i32::from(src.get(x as usize, y as usize));
    for i in 0..hd {
        let yu = (scy * i as f32) as i32;
        let yl = (f64::from(scy) * (i as f64 + 1.0)) as i32;
        let (yup, yuf, ylp, ylf) = (yu >> 4, yu & 0x0f, yl >> 4, yl & 0x0f);
        let dely = ylp - yup;
        for j in 0..wd {
            let xu = (scx * j as f32) as i32;
            let xl = (f64::from(scx) * (j as f64 + 1.0)) as i32;
            let (xup, xuf, xlp, xlf) = (xu >> 4, xu & 0x0f, xl >> 4, xl & 0x0f);
            let delx = xlp - xup;
            if xlp > wm2 || ylp > hm2 {
                d.set(j, i, px(xup, yup) as u8);
                continue;
            }
            let area = ((16 - xuf) + 16 * (delx - 1) + xlf) * ((16 - yuf) + 16 * (dely - 1) + ylf);
            let v00 = (16 - xuf) * (16 - yuf) * px(xup, yup);
            let v10 = xlf * (16 - yuf) * px(xlp, yup);
            let v01 = (16 - xuf) * ylf * px(xup, yup + dely);
            let v11 = xlf * ylf * px(xlp, yup + dely);
            let mut vin = 0;
            for k in 1..dely {
                for m in 1..delx {
                    vin += 256 * px(xup + m, yup + k);
                }
            }
            let mut vmid = 0;
            for k in 1..dely {
                vmid += (16 - xuf) * 16 * px(xup, yup + k);
            }
            for k in 1..dely {
                vmid += xlf * 16 * px(xlp, yup + k);
            }
            for m in 1..delx {
                vmid += 16 * (16 - yuf) * px(xup + m, yup);
            }
            for m in 1..delx {
                vmid += 16 * ylf * px(xup + m, yup + dely);
            }
            let val = (v00 + v01 + v10 + v11 + vin + vmid + 128) / area;
            d.set(j, i, val as u8);
        }
    }
    d
}

fn area_map2(src: &Gray) -> Gray {
    let (wd, hd) = (src.width / 2, src.height / 2);
    let mut d = Gray::new(wd, hd);
    for i in 0..hd {
        for j in 0..wd {
            let v = u32::from(src.get(2 * j, 2 * i))
                + u32::from(src.get(2 * j + 1, 2 * i))
                + u32::from(src.get(2 * j, 2 * i + 1))
                + u32::from(src.get(2 * j + 1, 2 * i + 1));
            d.set(j, i, (v >> 2) as u8);
        }
    }
    d
}

/// `pixScaleSmooth` for 8 bpp (scale < 0.02 only).
fn scale_smooth(src: &Gray, scalex: f32, scaley: f32) -> Gray {
    let minscale = scalex.min(scaley);
    let size = (1.0f64 / f64::from(minscale)) as f32;
    let isize = ((size + 0.5) as i32).clamp(2, 10000);
    let (ws, hs) = (src.width as i32, src.height as i32);
    if ws < isize || hs < isize {
        return src.clone();
    }
    let wd = ((scalex * ws as f32 + 0.5) as i32).max(1) as usize;
    let hd = ((scaley * hs as f32 + 0.5) as i32).max(1) as usize;
    let mut d = Gray::new(wd, hd);
    let norm = (1.0f64 / f64::from((isize * isize) as f32)) as f32;
    let wratio = ws as f32 / wd as f32;
    let hratio = hs as f32 / hd as f32;
    for i in 0..hd {
        let ys = ((hratio * i as f32) as i32).min(hs - isize);
        for j in 0..wd {
            let xs = ((wratio * j as f32) as i32).min(ws - isize);
            let mut v = 0i32;
            for m in 0..isize {
                for n in 0..isize {
                    v += i32::from(src.get((xs + n) as usize, (ys + m) as usize));
                }
            }
            d.set(j, i, (v as f32 * norm) as i32 as u8);
        }
    }
    d
}

/// `pixUnsharpMaskingGray2D` (halfwidth 1 or 2; border pixels copied).
fn unsharp_mask(src: &Gray, halfwidth: usize, fract: f32) -> Gray {
    let (w, h) = (src.width, src.height);
    let mut d = Gray::new(w, h);
    // pixCopyBorder: border rows/columns of width `halfwidth` from src,
    // interior zeroed (then fully overwritten below when w,h allow).
    for y in 0..h {
        for x in 0..w {
            if x < halfwidth || y < halfwidth || x + halfwidth >= w || y + halfwidth >= h {
                d.set(x, y, src.get(x, y));
            }
        }
    }
    let mut f = vec![0.0f32; w * h];
    for i in 0..h {
        for j in halfwidth..w.saturating_sub(halfwidth) {
            let mut val = 0i32;
            for k in j - halfwidth..=j + halfwidth {
                val += i32::from(src.get(k, i));
            }
            f[i * w + j] = val as f32;
        }
    }
    let norm: f32 = if halfwidth == 1 {
        (1.0f64 / 9.0) as f32
    } else {
        (1.0f64 / 25.0) as f32
    };
    for i in halfwidth..h.saturating_sub(halfwidth) {
        for j in halfwidth..w.saturating_sub(halfwidth) {
            let mut sum = 0.0f32;
            for k in i - halfwidth..=i + halfwidth {
                sum += f[k * w + j];
            }
            let val = norm * sum;
            let sval = i32::from(src.get(j, i));
            let x: f32 = sval as f32 + fract * (sval as f32 - val);
            let ival = (f64::from(x) + 0.5) as i32;
            d.set(j, i, ival.clamp(0, 255) as u8);
        }
    }
    d
}

/// An image as Tesseract holds it before recognition: 8 bpp grey, or RGB
/// (32 bpp; alpha already composited). Colour lines are scaled per channel
/// and only then reduced to grey, as `LSTMRecognizer` does with
/// `pix_original_`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pix {
    Gray(Gray),
    Rgb([Gray; 3]),
}

impl Pix {
    pub fn width(&self) -> usize {
        match self {
            Pix::Gray(g) => g.width,
            Pix::Rgb(p) => p[0].width,
        }
    }

    pub fn height(&self) -> usize {
        match self {
            Pix::Gray(g) => g.height,
            Pix::Rgb(p) => p[0].height,
        }
    }

    /// From interleaved RGB (3) or RGBA (4) bytes; alpha is blended over
    /// white like `pixRemoveAlpha` (Tesseract does this for PNG input).
    pub fn from_rgb(width: usize, height: usize, data: &[u8], channels: usize) -> Pix {
        let mut planes = [
            Gray::new(width, height),
            Gray::new(width, height),
            Gray::new(width, height),
        ];
        for i in 0..width * height {
            let px = &data[i * channels..(i + 1) * channels];
            for c in 0..3 {
                let s = px[c];
                planes[c].data[i] = if channels == 4 {
                    blend_over_white(s, px[3])
                } else {
                    s
                };
            }
        }
        Pix::Rgb(planes)
    }

    /// Grey+alpha: Leptonica expands to RGBA, then blends over white.
    pub fn from_gray_alpha(width: usize, height: usize, data: &[u8]) -> Pix {
        let mut g = Gray::new(width, height);
        for i in 0..width * height {
            g.data[i] = blend_over_white(data[2 * i], data[2 * i + 1]);
        }
        let planes = [g.clone(), g.clone(), g];
        Pix::Rgb(planes)
    }

    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Pix {
        match self {
            Pix::Gray(g) => Pix::Gray(g.crop(x, y, w, h)),
            Pix::Rgb(p) => Pix::Rgb([
                p[0].crop(x, y, w, h),
                p[1].crop(x, y, w, h),
                p[2].crop(x, y, w, h),
            ]),
        }
    }

    pub fn scale(&self, sx: f32, sy: f32) -> Pix {
        match self {
            Pix::Gray(g) => Pix::Gray(scale(g, sx, sy)),
            Pix::Rgb(p) => Pix::Rgb([
                scale(&p[0], sx, sy),
                scale(&p[1], sx, sy),
                scale(&p[2], sx, sy),
            ]),
        }
    }

    pub fn invert(&mut self) {
        match self {
            Pix::Gray(g) => g.invert(),
            Pix::Rgb(p) => p.iter_mut().for_each(Gray::invert),
        }
    }

    /// `pixConvertTo8` (luminance for RGB).
    pub fn to_gray(&self) -> Gray {
        match self {
            Pix::Gray(g) => g.clone(),
            Pix::Rgb([r, g, b]) => {
                let mut out = Gray::new(r.width, r.height);
                for i in 0..out.data.len() {
                    out.data[i] = luminance(r.data[i], g.data[i], b.data[i]);
                }
                out
            }
        }
    }
}

/// `pixBlendWithGrayMask` over a white background.
fn blend_over_white(s: u8, alpha: u8) -> u8 {
    if alpha == 0 {
        return 255;
    }
    let fract = (f64::from(alpha) / 255.0) as f32;
    let fs: f32 = fract * f32::from(s);
    ((1.0 - f64::from(fract)) * 255.0 + f64::from(fs)) as i32 as u8
}

impl Pix {
    /// From a decoded image as Leptonica would hold it. CMYK is not
    /// accepted here (see [`Pix::from_cmyk`]).
    pub fn from_image(img: &photo_core::Image) -> Option<Pix> {
        use photo_core::PixelFormat::*;
        let (w, h) = (img.width as usize, img.height as usize);
        Some(match img.format {
            Gray8 => Pix::Gray(Gray {
                width: w,
                height: h,
                data: img.data.clone(),
            }),
            GrayAlpha8 => Pix::from_gray_alpha(w, h, &img.data),
            Rgb8 => Pix::from_rgb(w, h, &img.data, 3),
            Rgba8 => Pix::from_rgb(w, h, &img.data, 4),
            Cmyk8 => return None,
        })
    }

    /// Leptonica's JPEG CMYK→RGB (`pixReadStreamJpeg`). `data` is CMYK as
    /// photo-jpeg returns it (Adobe inversion undone, i.e. `255 - stored`);
    /// `adobe` is whether the file has an Adobe APP14 marker.
    pub fn from_cmyk(width: usize, height: usize, data: &[u8], adobe: bool) -> Pix {
        let mut planes = [
            Gray::new(width, height),
            Gray::new(width, height),
            Gray::new(width, height),
        ];
        for i in 0..width * height {
            let p = &data[i * 4..i * 4 + 4];
            let stored = |v: u8| 255 - i32::from(v);
            let k = stored(p[3]);
            for c in 0..3 {
                let s = stored(p[c]);
                let v = if adobe {
                    (k * s) / 255
                } else {
                    k * (255 - s) / 255
                };
                planes[c].data[i] = v.clamp(0, 255) as u8;
            }
        }
        Pix::Rgb(planes)
    }
}
