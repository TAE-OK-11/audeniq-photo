//! Least-squares fitters and the quadratic spline (`linlsq.cpp`,
//! `quadlsq.cpp`, `quspline.cpp`, `quadratc.h`).

use super::f80::F80;
use std::ops::{Add, Div, Mul, Sub};

/// `LLSQ`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Llsq {
    pub total_weight: f64,
    sigx: f64,
    sigy: f64,
    sigxx: f64,
    sigxy: f64,
    sigyy: f64,
}

impl Llsq {
    pub fn add(&mut self, x: f64, y: f64) {
        self.total_weight += 1.0;
        self.sigx += x;
        self.sigy += y;
        self.sigxx += x * x;
        self.sigxy += x * y;
        self.sigyy += y * y;
    }

    pub fn covariance(&self) -> f64 {
        if self.total_weight > 0.0 {
            (self.sigxy - self.sigx * self.sigy / self.total_weight) / self.total_weight
        } else {
            0.0
        }
    }

    pub fn x_variance(&self) -> f64 {
        if self.total_weight > 0.0 {
            (self.sigxx - self.sigx * self.sigx / self.total_weight) / self.total_weight
        } else {
            0.0
        }
    }

    pub fn m(&self) -> f64 {
        let covar = self.covariance();
        let x_var = self.x_variance();
        if x_var != 0.0 { covar / x_var } else { 0.0 }
    }

    pub fn c(&self, m: f64) -> f64 {
        if self.total_weight > 0.0 {
            (self.sigy - m * self.sigx) / self.total_weight
        } else {
            0.0
        }
    }

    pub fn rms(&self, m: f64, c: f64) -> f64 {
        if self.total_weight > 0.0 {
            let error = self.sigyy
                + m * (m * self.sigxx + 2.0 * (c * self.sigx - self.sigxy))
                + c * (self.total_weight * c - 2.0 * self.sigy);
            if error >= 0.0 {
                (error / self.total_weight).sqrt()
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    /// `mean_point` as an `FCOORD`.
    pub fn mean_point(&self) -> (f32, f32) {
        if self.total_weight > 0.0 {
            (
                (self.sigx / self.total_weight) as f32,
                (self.sigy / self.total_weight) as f32,
            )
        } else {
            (0.0, 0.0)
        }
    }
}

/// `QLSQ`: quadratic least squares with x87 extended accumulators.
#[derive(Clone, Copy, Debug)]
pub struct Qlsq {
    n: i32,
    pub a: f64,
    pub b: f64,
    pub c: f64,
    sigx: f64,
    sigy: f64,
    sigxx: f64,
    sigxy: f64,
    sigyy: f64,
    sigxxx: F80,
    sigxxy: F80,
    sigxxxx: F80,
}

impl Default for Qlsq {
    fn default() -> Self {
        Qlsq {
            n: 0,
            a: 0.0,
            b: 0.0,
            c: 0.0,
            sigx: 0.0,
            sigy: 0.0,
            sigxx: 0.0,
            sigxy: 0.0,
            sigyy: 0.0,
            sigxxx: F80::ZERO,
            sigxxy: F80::ZERO,
            sigxxxx: F80::ZERO,
        }
    }
}

impl Qlsq {
    pub fn add(&mut self, x: f64, y: f64) {
        self.n += 1;
        self.sigx += x;
        self.sigy += y;
        self.sigxx += x * x;
        self.sigxy += x * y;
        self.sigyy += y * y;
        let xl = F80::from_f64(x);
        let yl = F80::from_f64(y);
        let xx = xl.mul(xl);
        self.sigxxx = self.sigxxx.add(xx.mul(xl));
        self.sigxxy = self.sigxxy.add(xx.mul(yl));
        self.sigxxxx = self.sigxxxx.add(xx.mul(xl).mul(xl));
    }

    pub fn fit(&mut self, degree: i32) {
        let n = F80::from_i64(i64::from(self.n));
        let sigxx = F80::from_f64(self.sigxx);
        let sigx = F80::from_f64(self.sigx);
        let sigy = F80::from_f64(self.sigy);
        let sigxy = F80::from_f64(self.sigxy);
        let x_variance = sigxx.mul(n).sub(sigx.mul(sigx));
        let min_var = F80::from_f64(1.0 / 1024.0);
        if x_variance.lt(min_var.mul(n).mul(n)) || degree < 1 || self.n < 2 {
            self.a = 0.0;
            self.b = 0.0;
            self.c = if self.n >= 1 && degree >= 0 {
                self.sigy / f64::from(self.n)
            } else {
                0.0
            };
            return;
        }
        let mut top96 = F80::ZERO;
        let mut bottom96 = F80::ZERO;
        let cubevar = self.sigxxx.mul(n).sub(sigxx.mul(sigx));
        let covariance = sigxy.mul(n).sub(sigx.mul(sigy));
        if self.n >= 4 && degree >= 2 {
            top96 = cubevar.mul(covariance);
            top96 = top96.add(x_variance.mul(sigxx.mul(sigy).sub(self.sigxxy.mul(n))));
            bottom96 = cubevar.mul(cubevar);
            bottom96 = bottom96.sub(x_variance.mul(self.sigxxxx.mul(n).sub(sigxx.mul(sigxx))));
        }
        if bottom96.ge(min_var.mul(n).mul(n).mul(n).mul(n)) {
            self.a = top96.div(bottom96).to_f64();
            let top = covariance.sub(cubevar.mul(F80::from_f64(self.a)));
            self.b = top.div(x_variance).to_f64();
        } else {
            self.a = 0.0;
            self.b = covariance.div(x_variance).to_f64();
        }
        self.c = (self.sigy - self.a * self.sigxx - self.b * self.sigx) / f64::from(self.n);
    }
}

/// `QUAD_COEFFS`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quad {
    pub a: f64,
    pub b: f32,
    pub c: f32,
}

impl Quad {
    pub fn y(&self, x: f32) -> f32 {
        ((self.a * f64::from(x) + f64::from(self.b)) * f64::from(x) + f64::from(self.c)) as f32
    }

    /// `QUAD_COEFFS::move`.
    pub fn move_by(&mut self, vx: i32, vy: i32) {
        let p = f64::from(vx as i16);
        let q = f64::from(vy as i16);
        self.c = (f64::from(self.c - self.b * p as f32) + self.a * p * p + q) as f32;
        self.b = (f64::from(self.b) - 2.0 * self.a * p) as f32;
    }
}

/// `QSPLINE`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QSpline {
    pub xcoords: Vec<i32>,
    pub quads: Vec<Quad>,
}

impl QSpline {
    pub fn segments(&self) -> i32 {
        self.quads.len() as i32
    }

    /// `QSPLINE(count, xstarts, coeffs)`.
    pub fn from_coeffs(xstarts: &[i32], coeffs: &[f64]) -> QSpline {
        let count = coeffs.len() / 3;
        QSpline {
            xcoords: xstarts[..=count].to_vec(),
            quads: (0..count)
                .map(|i| Quad {
                    a: coeffs[i * 3],
                    b: coeffs[i * 3 + 1] as f32,
                    c: coeffs[i * 3 + 2] as f32,
                })
                .collect(),
        }
    }

    /// `QSPLINE(xstarts, segcount, xpts, ypts, pointcount, degree)`.
    pub fn fit(
        xstarts: &[i32],
        segcount: usize,
        xpts: &[i32],
        ypts: &[i32],
        pointcount: usize,
        degree: i32,
    ) -> QSpline {
        let mut ptcounts = vec![0i32; segcount + 1];
        let mut segment = 0usize;
        for pi in 0..pointcount {
            while segment < segcount && xpts[pi] >= xstarts[segment] {
                segment += 1;
                ptcounts[segment] = ptcounts[segment - 1];
            }
            ptcounts[segment] += 1;
        }
        while segment < segcount {
            segment += 1;
            ptcounts[segment] = ptcounts[segment - 1];
        }
        let mut quads = Vec::with_capacity(segcount);
        for seg in 0..segcount {
            let mut q = Qlsq::default();
            let mut pi = ptcounts[seg] as usize;
            if pi > 0 && xpts[pi] != xpts[pi - 1] && xpts[pi] != xstarts[seg] {
                let y = ypts[pi - 1]
                    + (ypts[pi] - ypts[pi - 1]) * (xstarts[seg] - xpts[pi - 1])
                        / (xpts[pi] - xpts[pi - 1]);
                q.add(f64::from(xstarts[seg]), f64::from(y));
            }
            while pi < ptcounts[seg + 1] as usize {
                q.add(f64::from(xpts[pi]), f64::from(ypts[pi]));
                pi += 1;
            }
            if pi > 0 && pi < pointcount && xpts[pi] != xstarts[seg + 1] {
                let y = ypts[pi - 1]
                    + (ypts[pi] - ypts[pi - 1]) * (xstarts[seg + 1] - xpts[pi - 1])
                        / (xpts[pi] - xpts[pi - 1]);
                q.add(f64::from(xstarts[seg + 1]), f64::from(y));
            }
            q.fit(degree);
            quads.push(Quad {
                a: q.a,
                b: q.b as f32,
                c: q.c as f32,
            });
        }
        QSpline {
            xcoords: xstarts[..=segcount].to_vec(),
            quads,
        }
    }

    pub fn spline_index(&self, x: f64) -> usize {
        let mut bottom = 0usize;
        let mut top = self.quads.len();
        while top - bottom > 1 {
            let index = (top + bottom) / 2;
            if x >= f64::from(self.xcoords[index]) {
                bottom = index;
            } else {
                top = index;
            }
        }
        bottom
    }

    pub fn y(&self, x: f64) -> f64 {
        let i = self.spline_index(x);
        f64::from(self.quads[i].y(x as f32))
    }

    pub fn step(&self, x1: f64, x2: f64) -> f64 {
        let mut i1 = self.spline_index(x1);
        let i2 = self.spline_index(x2);
        let mut total = 0.0;
        while i1 < i2 {
            let xc = self.xcoords[i1 + 1] as f32;
            total += f64::from(self.quads[i1 + 1].y(xc));
            total -= f64::from(self.quads[i1].y(xc));
            i1 += 1;
        }
        total
    }

    pub fn move_by(&mut self, vx: i32, vy: i32) {
        let xs = i32::from(vx as i16);
        for (i, q) in self.quads.iter_mut().enumerate() {
            self.xcoords[i] += xs;
            q.move_by(vx, vy);
        }
        let n = self.quads.len();
        self.xcoords[n] += xs;
    }

    pub fn overlap(&self, s2: &QSpline, fraction: f64) -> bool {
        let segs = self.quads.len();
        let leftlimit = self.xcoords[1];
        let rightlimit = self.xcoords[segs - 1];
        let s2segs = s2.quads.len();
        !(s2segs < 3
            || f64::from(s2.xcoords[1])
                > f64::from(leftlimit) + fraction * f64::from(rightlimit - leftlimit)
            || f64::from(s2.xcoords[s2segs - 1])
                < f64::from(rightlimit) - fraction * f64::from(rightlimit - leftlimit))
    }

    /// `QSPLINE::extrapolate`.
    pub fn extrapolate(&mut self, gradient: f64, xmin: i32, xmax: i32) {
        let segs = self.quads.len();
        let mut increment = i32::from(xmin < self.xcoords[0]);
        if xmax > self.xcoords[segs] {
            increment += 1;
        }
        if increment == 0 {
            return;
        }
        let mut xs = Vec::with_capacity(segs + 1 + increment as usize);
        let mut qs = Vec::with_capacity(segs + increment as usize);
        if xmin < self.xcoords[0] {
            xs.push(xmin);
            let b = gradient as f32;
            let c =
                (self.y(f64::from(self.xcoords[0])) - f64::from(b * self.xcoords[0] as f32)) as f32;
            qs.push(Quad { a: 0.0, b, c });
        }
        for i in 0..segs {
            xs.push(self.xcoords[i]);
            qs.push(self.quads[i]);
        }
        xs.push(self.xcoords[segs]);
        if xmax > self.xcoords[segs] {
            let b = gradient as f32;
            let c = (self.y(f64::from(self.xcoords[segs]))
                - f64::from(b * self.xcoords[segs] as f32)) as f32;
            qs.push(Quad { a: 0.0, b, c });
            xs.push(xmax + 1);
        }
        self.xcoords = xs;
        self.quads = qs;
    }
}
