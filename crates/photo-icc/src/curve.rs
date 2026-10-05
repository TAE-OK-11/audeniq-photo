//! Tone curves: `curv` tables/gammas and `para` parametric functions,
//! evaluated like lcms (`cmsEvalToneCurveFloat`).

use photo_core::{Bytes, Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Curve {
    Identity,
    Gamma(f64),
    Table(Vec<u16>),
    /// ICC function type 0..=4 with its parameters (g, a, b, c, d, e, f).
    Parametric(u16, [f64; 7]),
    /// Inverse of a parametric curve (lcms negative types).
    InverseParametric(u16, [f64; 7]),
}

/// lcms `_cmsQuickSaturateWord`.
pub(crate) fn saturate_word(d: f64) -> u16 {
    let d = d + 0.5;
    if d <= 0.0 {
        0
    } else if d >= 65535.0 {
        65535
    } else {
        d.floor() as u16
    }
}

impl Curve {
    /// Parse a `curv` or `para` element. Returns the curve and its byte size.
    pub fn parse(d: &[u8]) -> Result<(Curve, usize)> {
        let mut b = Bytes::new(d);
        let kind: [u8; 4] = b.array()?;
        b.skip(4)?;
        match &kind {
            b"curv" => {
                let n = b.u32_be()? as usize;
                if n > 65536 {
                    return Err(Error::Limit("curve size"));
                }
                let c = match n {
                    0 => Curve::Identity,
                    1 => Curve::Gamma(f64::from(b.u16_be()?) / 256.0),
                    _ => {
                        let mut t = Vec::with_capacity(n);
                        for _ in 0..n {
                            t.push(b.u16_be()?);
                        }
                        Curve::Table(t)
                    }
                };
                Ok((c, 12 + 2 * n))
            }
            b"para" => {
                let ty = b.u16_be()?;
                b.skip(2)?;
                let n = match ty {
                    0 => 1,
                    1 => 3,
                    2 => 4,
                    3 => 5,
                    4 => 7,
                    _ => return Err(Error::Unsupported("parametric curve type")),
                };
                let mut p = [0f64; 7];
                for v in p.iter_mut().take(n) {
                    *v = f64::from(b.u32_be()? as i32) / 65536.0;
                }
                Ok((Curve::Parametric(ty, p), 12 + 4 * n))
            }
            _ => Err(Error::Invalid("curve type")),
        }
    }

    pub fn eval(&self, x: f64) -> f64 {
        match self {
            Curve::Identity => x,
            Curve::Gamma(g) => {
                if x <= 0.0 {
                    0.0
                } else {
                    x.powf(*g)
                }
            }
            Curve::Table(t) => {
                // 16-bit evaluation as lcms does for tabulated curves.
                let v = saturate_word(x * 65535.0);
                f64::from(lerp16(t, v)) / 65535.0
            }
            Curve::Parametric(ty, p) => parametric(*ty, p, x),
            Curve::InverseParametric(ty, p) => inverse_parametric(*ty, p, x),
        }
    }

    pub fn inverse(&self) -> Curve {
        match self {
            Curve::Identity => Curve::Identity,
            Curve::Gamma(g) => Curve::Gamma(if *g == 0.0 { 1.0 } else { 1.0 / g }),
            Curve::Parametric(ty, p) => Curve::InverseParametric(*ty, *p),
            Curve::InverseParametric(ty, p) => Curve::Parametric(*ty, *p),
            Curve::Table(t) => Curve::Table(invert_table(t)),
        }
    }
}

/// lcms `LinLerp1D` on a 16-bit table.
pub(crate) fn lerp16(t: &[u16], v: u16) -> u16 {
    let n = t.len();
    if v == 0xFFFF {
        return t[n - 1];
    }
    let val = (n as u64 - 1) * u64::from(v);
    let cell0 = (val / 65535) as usize;
    let rest = (val % 65535) as f64 / 65535.0;
    let cell1 = (cell0 + 1).min(n - 1);
    let y0 = f64::from(t[cell0]);
    let y1 = f64::from(t[cell1]);
    (y0 + (y1 - y0) * rest + 0.5).floor().clamp(0.0, 65535.0) as u16
}

fn invert_table(t: &[u16]) -> Vec<u16> {
    // Sample the inverse on 4096 points by searching the (assumed
    // monotonic) forward table, as lcms's cmsReverseToneCurve does.
    let n = 4096;
    let ascending = t.first() <= t.last();
    (0..n)
        .map(|i| {
            let y = (i as f64 / (n - 1) as f64) * 65535.0;
            let x = t
                .windows(2)
                .enumerate()
                .find_map(|(j, w)| {
                    let (a, b) = (f64::from(w[0]), f64::from(w[1]));
                    (y >= a.min(b) && y <= a.max(b)).then(|| {
                        let f = if b == a { 0.0 } else { (y - a) / (b - a) };
                        (j as f64 + f) / (t.len() - 1) as f64
                    })
                })
                .unwrap_or(if (y < f64::from(t[0])) == ascending {
                    0.0
                } else {
                    1.0
                });
            saturate_word(x * 65535.0)
        })
        .collect()
}

fn parametric(ty: u16, p: &[f64; 7], x: f64) -> f64 {
    let (g, a, b, c, d, e, f) = (p[0], p[1], p[2], p[3], p[4], p[5], p[6]);
    let pw = |v: f64| if v <= 0.0 { 0.0 } else { v.powf(g) };
    match ty {
        0 => pw(x),
        1 => {
            if a == 0.0 {
                return 0.0;
            }
            if x >= -b / a { pw(a * x + b) } else { 0.0 }
        }
        2 => {
            if a == 0.0 {
                return c;
            }
            if x >= -b / a { pw(a * x + b) + c } else { c }
        }
        3 => {
            if x >= d {
                pw(a * x + b)
            } else {
                c * x
            }
        }
        _ => {
            if x >= d {
                pw(a * x + b) + e
            } else {
                c * x + f
            }
        }
    }
}

fn inverse_parametric(ty: u16, p: &[f64; 7], y: f64) -> f64 {
    let (g, a, b, c, d, e, f) = (p[0], p[1], p[2], p[3], p[4], p[5], p[6]);
    let root = |v: f64| {
        if v <= 0.0 || g == 0.0 {
            0.0
        } else {
            v.powf(1.0 / g)
        }
    };
    match ty {
        0 => root(y),
        1 => {
            if a == 0.0 {
                0.0
            } else {
                (root(y) - b) / a
            }
        }
        2 => {
            if a == 0.0 {
                0.0
            } else {
                (root(y - c) - b) / a
            }
        }
        3 => {
            let disc = if a * d + b <= 0.0 {
                0.0
            } else {
                (a * d + b).powf(g)
            };
            if y >= disc {
                if a == 0.0 { 0.0 } else { (root(y) - b) / a }
            } else if c == 0.0 {
                0.0
            } else {
                y / c
            }
        }
        _ => {
            let t = a * d + b;
            let disc = if t <= 0.0 { e } else { t.powf(g) + e };
            if y >= disc {
                if a == 0.0 { 0.0 } else { (root(y - e) - b) / a }
            } else if c == 0.0 {
                0.0
            } else {
                (y - f) / c
            }
        }
    }
}
