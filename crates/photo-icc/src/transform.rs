//! Device → sRGB transforms.

use crate::curve::{Curve, saturate_word};
use crate::lut::{Clut, LabEncoding, Lut};
use crate::profile::Profile;
use crate::srgb::{self, Mat3, apply, inverse, mul};
use photo_core::{Error, Result};

const PERCEPTUAL_BLACK: [f64; 3] = [0.00336, 0.0034731, 0.00287];

/// lcms `FROM_16_TO_8`.
#[inline]
fn from16(v: u16) -> u8 {
    ((u32::from(v) * 65281 + 8_388_608) >> 24) as u8
}

/// `DOUBLE_TO_1FIXED14`.
fn fixed14(x: f64) -> i32 {
    (x * 16384.0 + 0.5).floor() as i32
}

/// A prepared conversion from an embedded profile's device space to sRGB.
pub struct Transform {
    kind: Kind,
    channels: usize,
}

enum Kind {
    /// lcms OptimizeMatrixShaper (8-bit, 1.14 fixed point).
    MatShaper {
        shaper1: Box<[[i32; 256]; 3]>,
        mat: [[i32; 3]; 3],
        off: [i32; 3],
        shaper2: Vec<u8>,
    },
    /// Per-value table (gray input), from the resampled CLUT.
    Gray(Box<[[u8; 3]; 256]>),
    /// Resampled CLUT (lcms OptimizeByResampling), 16-bit.
    Clut(Clut),
}

/// Input side of the pipeline: device values → PCS XYZ (D50, relative).
enum Input {
    MatShaper {
        curves: [Curve; 3],
        m: Mat3,
    },
    /// Gray TRC; with a Lab PCS the curve yields L* (lcms
    /// BuildGrayInputMatrixPipeline), otherwise Y.
    Gray(Curve, bool),
    Lut {
        lut: Box<Lut>,
        lab: Option<LabEncoding>,
    },
}

impl Input {
    fn from_profile(p: &Profile) -> Result<Input> {
        let pcs_lab = match &p.pcs {
            b"Lab " => true,
            b"XYZ " => false,
            _ => return Err(Error::Unsupported("ICC PCS")),
        };
        // lcms _cmsReadInputLUT: A2B[perceptual] = A2B0, then matrix-shaper.
        if let Some(lut) = p.lut(b"A2B0") {
            let lut = lut?;
            if lut.inputs != p.channels().ok_or(Error::Unsupported("ICC color space"))?
                || lut.outputs != 3
            {
                return Err(Error::Invalid("A2B0 channel count"));
            }
            return Ok(Input::Lut {
                lut: Box::new(lut.clone()),
                lab: pcs_lab.then_some(lut.lab),
            });
        }
        match &p.color_space {
            b"RGB " => {
                let r = p.xyz(b"rXYZ")?;
                let g = p.xyz(b"gXYZ")?;
                let b = p.xyz(b"bXYZ")?;
                let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
                Ok(Input::MatShaper {
                    curves: [p.curve(b"rTRC")?, p.curve(b"gTRC")?, p.curve(b"bTRC")?],
                    m,
                })
            }
            b"GRAY" => Ok(Input::Gray(p.curve(b"kTRC")?, pcs_lab)),
            _ => Err(Error::Unsupported(
                "ICC profile without A2B0 for this color space",
            )),
        }
    }

    fn to_xyz(&self, v: &[f64]) -> [f64; 3] {
        match self {
            Input::MatShaper { curves, m } => apply(
                m,
                [
                    curves[0].eval(v[0]),
                    curves[1].eval(v[1]),
                    curves[2].eval(v[2]),
                ],
            ),
            Input::Gray(c, false) => {
                let y = c.eval(v[0]);
                [y * srgb::D50[0], y * srgb::D50[1], y * srgb::D50[2]]
            }
            Input::Gray(c, true) => srgb::lab_to_xyz([c.eval(v[0]) * 100.0, 0.0, 0.0]),
            Input::Lut { lut, lab } => {
                let mut o = [0f64; 16];
                lut.eval(v, &mut o);
                match lab {
                    Some(LabEncoding::V2) => {
                        let w = |x: f64| x * 65535.0;
                        srgb::lab_to_xyz([
                            w(o[0]) * 100.0 / 65280.0,
                            w(o[1]) / 256.0 - 128.0,
                            w(o[2]) / 256.0 - 128.0,
                        ])
                    }
                    Some(LabEncoding::V4) => {
                        srgb::lab_to_xyz([o[0] * 100.0, o[1] * 255.0 - 128.0, o[2] * 255.0 - 128.0])
                    }
                    None => {
                        let s = 65535.0 / 32768.0;
                        [o[0] * s, o[1] * s, o[2] * s]
                    }
                }
            }
        }
    }
}

/// lcms ComputeBlackPointCompensation: XYZ' = a*XYZ + b, mapping the input
/// black point to the (zero) sRGB black and keeping D50 white.
fn bpc(black_in: [f64; 3]) -> Option<([f64; 3], [f64; 3])> {
    if black_in.iter().all(|&v| v.abs() < 1e-12) {
        return None;
    }
    let mut a = [0.0; 3];
    let mut b = [0.0; 3];
    for i in 0..3 {
        let t = black_in[i] - srgb::D50[i];
        a[i] = (0.0 - srgb::D50[i]) / t;
        b[i] = -srgb::D50[i] * (0.0 - black_in[i]) / t;
    }
    Some((a, b))
}

impl Transform {
    /// Build the transform for a profile embedded in an image with
    /// `channels` color channels (1 gray, 3 RGB, 4 CMYK).
    pub fn to_srgb(profile: &Profile, channels: usize) -> Result<Transform> {
        if profile.channels() != Some(channels) {
            return Err(Error::Invalid("ICC color space does not match the image"));
        }
        let input = Input::from_profile(profile)?;
        let out_m = inverse(&srgb::matrix()).ok_or(Error::Invalid("sRGB matrix"))?;
        let out_trc = srgb::trc().inverse();

        // Black point (lcms cmsDetectBlackPoint, perceptual intent).
        let black = match &input {
            Input::Lut { .. } if profile.is_v4() => PERCEPTUAL_BLACK,
            _ => {
                let dark: Vec<f64> = match channels {
                    4 => vec![1.0; 4],
                    n => vec![0.0; n],
                };
                let mut lab = srgb::xyz_to_lab(input.to_xyz(&dark));
                lab[0] = lab[0].min(50.0);
                lab[1] = 0.0;
                lab[2] = 0.0;
                srgb::lab_to_xyz(lab)
            }
        };
        let bpc = bpc(black);

        let kind = match &input {
            Input::MatShaper { curves, m } if channels == 3 => {
                let mut total = mul(&out_m, m);
                let mut off = [0.0; 3];
                if let Some((a, b)) = bpc {
                    let scaled: Mat3 =
                        std::array::from_fn(|i| std::array::from_fn(|j| m[i][j] * a[i]));
                    total = mul(&out_m, &scaled);
                    off = apply(&out_m, b);
                }
                let mut shaper1 = Box::new([[0i32; 256]; 3]);
                for (c, s) in shaper1.iter_mut().enumerate() {
                    for (i, v) in s.iter_mut().enumerate() {
                        *v = fixed14(curves[c].eval(i as f64 / 255.0));
                    }
                }
                let mut shaper2 = vec![0u8; 16385];
                for (i, v) in shaper2.iter_mut().enumerate() {
                    let val = out_trc.eval(i as f64 / 16384.0).clamp(0.0, 1.0);
                    *v = from16(saturate_word(val * 65535.0));
                }
                Kind::MatShaper {
                    shaper1,
                    mat: std::array::from_fn(|i| std::array::from_fn(|j| fixed14(total[i][j]))),
                    off: std::array::from_fn(|i| fixed14(off[i])),
                    shaper2,
                }
            }
            _ => {
                let eval = |dev: &[f64]| -> [u16; 3] {
                    let mut xyz = input.to_xyz(dev);
                    if let Some((a, b)) = bpc {
                        for i in 0..3 {
                            xyz[i] = a[i] * xyz[i] + b[i];
                        }
                    }
                    let lin = apply(&out_m, xyz);
                    std::array::from_fn(|i| {
                        saturate_word(
                            out_trc.eval(lin[i].clamp(0.0, 1.0)).clamp(0.0, 1.0) * 65535.0,
                        )
                    })
                };
                {
                    // lcms _cmsReasonableGridpointsByColorspace defaults:
                    // 17 nodes per axis for CMYK, 33 otherwise.
                    let n: usize = if channels == 4 { 17 } else { 33 };
                    let grid = vec![n; channels];
                    let total = n.pow(channels as u32);
                    let mut table = vec![0u16; total * 3];
                    let mut dev = vec![0f64; channels];
                    for node in 0..total {
                        let mut rest = node;
                        for c in (0..channels).rev() {
                            let q = rest % n;
                            rest /= n;
                            let v16 = (q as f64 * 65535.0 / (n - 1) as f64 + 0.5).floor();
                            dev[c] = v16 / 65535.0;
                        }
                        let o = eval(&dev);
                        table[node * 3..node * 3 + 3].copy_from_slice(&o);
                    }
                    // lcms FixWhiteMisalignment: device white maps to sRGB white.
                    let white_node = if channels == 4 { 0 } else { total - 1 };
                    table[white_node * 3..white_node * 3 + 3].copy_from_slice(&[0xFFFF; 3]);
                    let clut = Clut::new(grid, 3, table)?;
                    // 8-bit input: precompute all 256 results for gray.
                    if channels == 1 {
                        let mut t = Box::new([[0u8; 3]; 256]);
                        let mut out = [0u16; 3];
                        for (i, px) in t.iter_mut().enumerate() {
                            clut.eval16(&[i as u16 * 257], &mut out);
                            *px = [from16(out[0]), from16(out[1]), from16(out[2])];
                        }
                        Kind::Gray(t)
                    } else {
                        Kind::Clut(clut)
                    }
                }
            }
        };
        Ok(Transform { kind, channels })
    }

    /// Number of input channels this transform expects.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Convert interleaved device pixels (`channels` per pixel) into RGB8.
    /// `src` and `dst` may describe the same number of pixels only.
    pub fn convert(&self, src: &[u8], dst: &mut [u8]) {
        let ch = self.channels;
        let n = (src.len() / ch).min(dst.len() / 3);
        match &self.kind {
            Kind::MatShaper {
                shaper1,
                mat,
                off,
                shaper2,
            } => {
                for i in 0..n {
                    let (r, g, b) = (
                        src[i * 3] as usize,
                        src[i * 3 + 1] as usize,
                        src[i * 3 + 2] as usize,
                    );
                    let v = [shaper1[0][r], shaper1[1][g], shaper1[2][b]];
                    for c in 0..3 {
                        let l = (mat[c][0] * v[0]
                            + mat[c][1] * v[1]
                            + mat[c][2] * v[2]
                            + off[c]
                            + 0x2000)
                            >> 14;
                        dst[i * 3 + c] = shaper2[l.clamp(0, 16384) as usize];
                    }
                }
            }
            Kind::Gray(t) => {
                for i in 0..n {
                    dst[i * 3..i * 3 + 3].copy_from_slice(&t[src[i] as usize]);
                }
            }
            Kind::Clut(clut) => {
                let mut inp = [0u16; 4];
                let mut out = [0u16; 3];
                for i in 0..n {
                    for c in 0..ch {
                        inp[c] = u16::from(src[i * ch + c]) * 257;
                    }
                    clut.eval16(&inp[..ch], &mut out);
                    for c in 0..3 {
                        dst[i * 3 + c] = from16(out[c]);
                    }
                }
            }
        }
    }

    /// In-place conversion for RGB input.
    pub fn convert_rgb_in_place(&self, px: &mut [u8]) -> Result<()> {
        if self.channels != 3 {
            return Err(Error::Invalid("not an RGB transform"));
        }
        // Process in chunks through a small buffer to keep it in place.
        let mut tmp = [0u8; 3 * 1024];
        for chunk in px.chunks_mut(3 * 1024) {
            let n = chunk.len();
            self.convert(chunk, &mut tmp[..n]);
            chunk.copy_from_slice(&tmp[..n]);
        }
        Ok(())
    }
}
