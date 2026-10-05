//! lcms `cmsCreate_sRGBProfile`: D65 from `cmsWhitePointFromTemp(6504)`,
//! Rec.709 primaries, Bradford-adapted to D50, type-4 parametric TRC.

use crate::curve::Curve;

pub(crate) const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

pub(crate) type Mat3 = [[f64; 3]; 3];

pub(crate) fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

pub(crate) fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// `_cmsMAT3inverse`.
pub(crate) fn inverse(a: &Mat3) -> Option<Mat3> {
    let c0 = a[1][1] * a[2][2] - a[1][2] * a[2][1];
    let c1 = -a[1][0] * a[2][2] + a[1][2] * a[2][0];
    let c2 = a[1][0] * a[2][1] - a[1][1] * a[2][0];
    let det = a[0][0] * c0 + a[0][1] * c1 + a[0][2] * c2;
    if det.abs() < 1e-12 {
        return None;
    }
    Some([
        [
            c0 / det,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) / det,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) / det,
        ],
        [
            c1 / det,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) / det,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) / det,
        ],
        [
            c2 / det,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) / det,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / det,
        ],
    ])
}

fn white_from_temp(t: f64) -> (f64, f64) {
    let (t2, t3) = (t * t, t * t * t);
    let x = if t <= 7000.0 {
        -4.6070 * (1e9 / t3) + 2.9678 * (1e6 / t2) + 0.09911 * (1e3 / t) + 0.244063
    } else {
        -2.0064 * (1e9 / t3) + 1.9018 * (1e6 / t2) + 0.24748 * (1e3 / t) + 0.237040
    };
    (x, -3.0 * x * x + 2.87 * x - 0.275)
}

/// Bradford chromatic adaptation from `from` to `to` (XYZ).
pub(crate) fn bradford(from: [f64; 3], to: [f64; 3]) -> Mat3 {
    let b: Mat3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let src = apply(&b, from);
    let dst = apply(&b, to);
    let cone = [
        [dst[0] / src[0], 0.0, 0.0],
        [0.0, dst[1] / src[1], 0.0],
        [0.0, 0.0, dst[2] / src[2]],
    ];
    mul(
        &inverse(&b).expect("Bradford is invertible"),
        &mul(&cone, &b),
    )
}

/// Linear sRGB → XYZ(D50) colorant matrix of lcms's built-in profile.
pub(crate) fn matrix() -> Mat3 {
    let (xn, yn) = white_from_temp(6504.0);
    let p = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];
    let prim: Mat3 = [
        [p[0].0, p[1].0, p[2].0],
        [p[0].1, p[1].1, p[2].1],
        [
            1.0 - p[0].0 - p[0].1,
            1.0 - p[1].0 - p[1].1,
            1.0 - p[2].0 - p[2].1,
        ],
    ];
    let inv = inverse(&prim).expect("primaries are independent");
    let white = [xn / yn, 1.0, (1.0 - xn - yn) / yn];
    let coef = apply(&inv, white);
    let mut m = prim;
    for row in m.iter_mut() {
        for (j, v) in row.iter_mut().enumerate() {
            *v *= coef[j];
        }
    }
    mul(&bradford(white, D50), &m)
}

pub(crate) fn trc() -> Curve {
    Curve::Parametric(
        4,
        [
            2.4,
            1.0 / 1.055,
            0.055 / 1.055,
            1.0 / 12.92,
            0.04045,
            0.0,
            0.0,
        ],
    )
}

pub(crate) fn lab_to_xyz(lab: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + 0.002 * lab[1];
    let fz = fy - 0.005 * lab[2];
    let f = |t: f64| {
        let lim = 24.0 / 116.0;
        if t <= lim {
            (108.0 / 841.0) * (t - 16.0 / 116.0)
        } else {
            t * t * t
        }
    };
    [f(fx) * D50[0], f(fy) * D50[1], f(fz) * D50[2]]
}

pub(crate) fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let f = |t: f64| {
        let lim = (24.0f64 / 116.0).powi(3);
        if t <= lim {
            (841.0 / 108.0) * t + 16.0 / 116.0
        } else {
            t.cbrt()
        }
    };
    let fx = f(xyz[0] / D50[0]);
    let fy = f(xyz[1] / D50[1]);
    let fz = f(xyz[2] / D50[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}
