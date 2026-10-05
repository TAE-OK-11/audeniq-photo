//! LUT-based transforms (lut8Type, lut16Type, lutAtoBType) and the lcms
//! 16-bit interpolation kernels (`cmsintrp.c`).

use crate::curve::{Curve, saturate_word};
use photo_core::{Bytes, Error, Result};

/// Interpolation tables bigger than this are refused.
const MAX_CLUT_ENTRIES: usize = 1 << 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LabEncoding {
    /// lut16Type: L 0..0xFF00 = 0..100 (ICC v2).
    V2,
    /// lut8Type and lutAtoBType: L 0..0xFFFF = 0..100.
    V4,
}

/// A 16-bit color lookup table with lcms's precomputed strides.
#[derive(Debug, Clone)]
pub(crate) struct Clut {
    pub inputs: usize,
    pub outputs: usize,
    pub grid: Vec<usize>,
    pub table: Vec<u16>,
    opta: Vec<usize>,
}

impl Clut {
    pub(crate) fn new(grid: Vec<usize>, outputs: usize, table: Vec<u16>) -> Result<Clut> {
        let inputs = grid.len();
        if inputs == 0 || inputs > 8 || outputs == 0 || outputs > 16 || grid.iter().any(|&g| g < 2)
        {
            return Err(Error::Invalid("CLUT shape"));
        }
        let mut opta = vec![0usize; inputs];
        opta[0] = outputs;
        for i in 1..inputs {
            opta[i] = opta[i - 1] * grid[inputs - i];
        }
        let n = grid
            .iter()
            .try_fold(outputs, |acc, &g| acc.checked_mul(g))
            .ok_or(Error::Limit("CLUT size"))?;
        if n != table.len() {
            return Err(Error::Invalid("CLUT size"));
        }
        Ok(Clut {
            inputs,
            outputs,
            grid,
            table,
            opta,
        })
    }

    /// Evaluate with 16-bit inputs, as lcms does for 16-bit tables.
    pub(crate) fn eval16(&self, input: &[u16], out: &mut [u16]) {
        match self.inputs {
            1 => self.eval1(input[0], out),
            3 => self.tetrahedral(input, 0, out, false),
            4 => self.eval4(input, out),
            _ => self.multilinear(input, out),
        }
    }

    /// lcms Eval1Input.
    fn eval1(&self, v: u16, out: &mut [u16]) {
        let domain = self.grid[0] - 1;
        let o = self.opta[0];
        if v == 0xFFFF {
            out[..self.outputs].copy_from_slice(&self.table[domain * o..domain * o + self.outputs]);
            return;
        }
        let (k0, rk, _) = self.fixed(v, 0);
        let (a, b) = (k0 * o, (k0 + 1) * o);
        for i in 0..self.outputs {
            let (l, h) = (i32::from(self.table[a + i]), i32::from(self.table[b + i]));
            out[i] = ((((h - l) * rk + 0x8000) >> 16) + l) as u16;
        }
    }

    fn fixed(&self, v: u16, axis: usize) -> (usize, i32, bool) {
        let domain = (self.grid[axis] - 1) as i32;
        let a = i32::from(v) * domain;
        let f = a + ((a + 0x7FFF) / 0xFFFF);
        ((f >> 16) as usize, f & 0xFFFF, v == 0xFFFF)
    }

    /// lcms TetrahedralInterp16 (base offset lets eval4 reuse it).
    fn tetrahedral(&self, input: &[u16], base: usize, out: &mut [u16], eval4_rounding: bool) {
        let n = self.inputs;
        let (x0, rx, xe) = self.fixed(input[0], n - 3);
        let (y0, ry, ye) = self.fixed(input[1], n - 2);
        let (z0, rz, ze) = self.fixed(input[2], n - 1);
        let (ox, oy, oz) = (self.opta[2], self.opta[1], self.opta[0]);
        let x0 = x0 * ox;
        let x1 = x0 + if xe { 0 } else { ox };
        let y0 = y0 * oy;
        let y1 = y0 + if ye { 0 } else { oy };
        let z0 = z0 * oz;
        let z1 = z0 + if ze { 0 } else { oz };
        let t = &self.table;
        for (c, o) in out.iter_mut().enumerate().take(self.outputs) {
            let d = |x: usize, y: usize, z: usize| i32::from(t[base + x + y + z + c]);
            let c0 = d(x0, y0, z0);
            let (c1, c2, c3) = if rx >= ry && ry >= rz {
                (
                    d(x1, y0, z0) - c0,
                    d(x1, y1, z0) - d(x1, y0, z0),
                    d(x1, y1, z1) - d(x1, y1, z0),
                )
            } else if rx >= rz && rz >= ry {
                (
                    d(x1, y0, z0) - c0,
                    d(x1, y1, z1) - d(x1, y0, z1),
                    d(x1, y0, z1) - d(x1, y0, z0),
                )
            } else if rz >= rx && rx >= ry {
                (
                    d(x1, y0, z1) - d(x0, y0, z1),
                    d(x1, y1, z1) - d(x1, y0, z1),
                    d(x0, y0, z1) - c0,
                )
            } else if ry >= rx && rx >= rz {
                (
                    d(x1, y1, z0) - d(x0, y1, z0),
                    d(x0, y1, z0) - c0,
                    d(x1, y1, z1) - d(x1, y1, z0),
                )
            } else if ry >= rz && rz >= rx {
                (
                    d(x1, y1, z1) - d(x0, y1, z1),
                    d(x0, y1, z0) - c0,
                    d(x0, y1, z1) - d(x0, y1, z0),
                )
            } else if rz >= ry && ry >= rx {
                (
                    d(x1, y1, z1) - d(x0, y1, z1),
                    d(x0, y1, z1) - d(x0, y0, z1),
                    d(x0, y0, z1) - c0,
                )
            } else {
                (0, 0, 0)
            };
            let rest = c1 * rx + c2 * ry + c3 * rz;
            *o = if eval4_rounding {
                let f = rest + ((rest + 0x7FFF) / 0xFFFF);
                (c0 + ((f + 0x8000) >> 16)) as u16
            } else {
                let rest = rest + 0x8001;
                (c0 + ((rest + (rest >> 16)) >> 16)) as u16
            };
        }
    }

    /// lcms Eval4Inputs: tetrahedral on the last three inputs at the two
    /// K planes, then linear interpolation in K.
    fn eval4(&self, input: &[u16], out: &mut [u16]) {
        let (k0, rk, ke) = self.fixed(input[0], 0);
        let ok = self.opta[3];
        let k0 = k0 * ok;
        let k1 = k0 + if ke { 0 } else { ok };
        let mut t1 = [0u16; 16];
        let mut t2 = [0u16; 16];
        self.tetrahedral(&input[1..4], k0, &mut t1, true);
        self.tetrahedral(&input[1..4], k1, &mut t2, true);
        for i in 0..self.outputs {
            let (l, h) = (i32::from(t1[i]), i32::from(t2[i]));
            out[i] = ((((h - l) * rk + 0x8000) >> 16) + l) as u16;
        }
    }

    fn multilinear(&self, input: &[u16], out: &mut [u16]) {
        let n = self.inputs;
        let mut acc = vec![0f64; self.outputs];
        let mut idx = vec![(0usize, 0usize, 0f64); n];
        for (i, slot) in idx.iter_mut().enumerate() {
            let pos = f64::from(input[i]) / 65535.0 * (self.grid[i] - 1) as f64;
            let lo = (pos.floor() as usize).min(self.grid[i] - 1);
            let hi = (lo + 1).min(self.grid[i] - 1);
            *slot = (lo, hi, pos - lo as f64);
        }
        for corner in 0..(1usize << n) {
            let mut w = 1.0;
            let mut off = 0;
            for (i, &(lo, hi, f)) in idx.iter().enumerate() {
                let bit = corner >> i & 1 == 1;
                w *= if bit { f } else { 1.0 - f };
                off += if bit { hi } else { lo } * self.opta[n - 1 - i];
            }
            if w == 0.0 {
                continue;
            }
            for (c, a) in acc.iter_mut().enumerate() {
                *a += w * f64::from(self.table[off + c]);
            }
        }
        for (o, a) in out.iter_mut().zip(acc) {
            *o = saturate_word(a);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Lut {
    pub inputs: usize,
    pub outputs: usize,
    pub a_curves: Vec<Curve>,
    pub clut: Option<Clut>,
    pub m_curves: Vec<Curve>,
    /// 3x3 matrix followed by a 3-element offset.
    pub matrix: Option<[f64; 12]>,
    pub b_curves: Vec<Curve>,
    pub lab: LabEncoding,
}

fn s15(b: &mut Bytes) -> Result<f64> {
    Ok(f64::from(b.u32_be()? as i32) / 65536.0)
}

impl Lut {
    pub(crate) fn parse(d: &[u8]) -> Result<Lut> {
        let mut b = Bytes::new(d);
        let kind: [u8; 4] = b.array()?;
        b.skip(4)?;
        match &kind {
            b"mft1" | b"mft2" => {
                let wide = &kind == b"mft2";
                let inputs = b.u8()? as usize;
                let outputs = b.u8()? as usize;
                let grid = b.u8()? as usize;
                b.skip(1)?;
                b.skip(36)?; // matrix: only meaningful for XYZ input
                let (n_in, n_out) = if wide {
                    (b.u16_be()? as usize, b.u16_be()? as usize)
                } else {
                    (256, 256)
                };
                if !(1..=8).contains(&inputs)
                    || !(1..=16).contains(&outputs)
                    || grid < 2
                    || n_in < 2
                    || n_out < 2
                    || n_in > 4096
                    || n_out > 4096
                {
                    return Err(Error::Invalid("lut16 shape"));
                }
                let mut read = |n: usize| -> Result<Vec<u16>> {
                    (0..n)
                        .map(|_| {
                            if wide {
                                b.u16_be()
                            } else {
                                b.u8().map(|v| u16::from(v) * 257)
                            }
                        })
                        .collect()
                };
                let a_curves = (0..inputs)
                    .map(|_| read(n_in).map(Curve::Table))
                    .collect::<Result<Vec<_>>>()?;
                let entries = (0..inputs)
                    .try_fold(outputs, |acc: usize, _| acc.checked_mul(grid))
                    .ok_or(Error::Limit("CLUT size"))?;
                if entries > MAX_CLUT_ENTRIES {
                    return Err(Error::Limit("CLUT size"));
                }
                let table = read(entries)?;
                let clut = Clut::new(vec![grid; inputs], outputs, table)?;
                let b_curves = (0..outputs)
                    .map(|_| read(n_out).map(Curve::Table))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Lut {
                    inputs,
                    outputs,
                    a_curves,
                    clut: Some(clut),
                    m_curves: Vec::new(),
                    matrix: None,
                    b_curves,
                    lab: if wide {
                        LabEncoding::V2
                    } else {
                        LabEncoding::V4
                    },
                })
            }
            b"mAB " => {
                let inputs = b.u8()? as usize;
                let outputs = b.u8()? as usize;
                b.skip(2)?;
                let off_b = b.u32_be()? as usize;
                let off_m = b.u32_be()? as usize;
                let off_mc = b.u32_be()? as usize;
                let off_clut = b.u32_be()? as usize;
                let off_a = b.u32_be()? as usize;
                if !(1..=8).contains(&inputs) || !(1..=16).contains(&outputs) {
                    return Err(Error::Invalid("lutAtoB shape"));
                }
                let curves = |off: usize, n: usize| -> Result<Vec<Curve>> {
                    if off == 0 {
                        return Ok(Vec::new());
                    }
                    let mut pos = off;
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        let (c, len) = Curve::parse(d.get(pos..).ok_or(Error::Truncated)?)?;
                        v.push(c);
                        pos += len.div_ceil(4) * 4;
                    }
                    Ok(v)
                };
                let b_curves = curves(off_b, outputs)?;
                let a_curves = curves(off_a, inputs)?;
                let m_curves = curves(off_mc, outputs)?;
                let matrix = if off_m != 0 {
                    let mut m = Bytes::at(d, off_m);
                    let mut v = [0f64; 12];
                    for x in v.iter_mut() {
                        *x = s15(&mut m)?;
                    }
                    Some(v)
                } else {
                    None
                };
                let clut = if off_clut != 0 {
                    let mut c = Bytes::at(d, off_clut);
                    let g: [u8; 16] = c.array()?;
                    let precision = c.u8()?;
                    c.skip(3)?;
                    let grid: Vec<usize> = g[..inputs].iter().map(|&x| x as usize).collect();
                    let entries = grid
                        .iter()
                        .try_fold(outputs, |acc: usize, &x| acc.checked_mul(x))
                        .ok_or(Error::Limit("CLUT size"))?;
                    if entries > MAX_CLUT_ENTRIES {
                        return Err(Error::Limit("CLUT size"));
                    }
                    let table = (0..entries)
                        .map(|_| match precision {
                            1 => c.u8().map(|v| u16::from(v) * 257),
                            2 => c.u16_be(),
                            _ => Err(Error::Invalid("CLUT precision")),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Some(Clut::new(grid, outputs, table)?)
                } else {
                    None
                };
                if clut
                    .as_ref()
                    .is_some_and(|c| c.inputs != inputs || c.outputs != outputs)
                {
                    return Err(Error::Invalid("lutAtoB CLUT shape"));
                }
                if clut.is_none() && inputs != outputs {
                    return Err(Error::Invalid("lutAtoB without CLUT"));
                }
                Ok(Lut {
                    inputs,
                    outputs,
                    a_curves,
                    clut,
                    m_curves,
                    matrix,
                    b_curves,
                    lab: LabEncoding::V4,
                })
            }
            _ => Err(Error::Unsupported("LUT tag type")),
        }
    }

    /// Evaluate in normalized floats, mirroring lcms's float pipeline
    /// (16-bit tables and CLUTs are evaluated at 16-bit precision).
    pub(crate) fn eval(&self, input: &[f64], out: &mut [f64]) {
        let mut v = [0f64; 16];
        v[..self.inputs].copy_from_slice(&input[..self.inputs]);
        for (x, c) in v.iter_mut().zip(&self.a_curves) {
            *x = c.eval(*x);
        }
        if let Some(clut) = &self.clut {
            let mut i16 = [0u16; 16];
            for i in 0..self.inputs {
                i16[i] = saturate_word(v[i] * 65535.0);
            }
            let mut o16 = [0u16; 16];
            clut.eval16(&i16[..self.inputs], &mut o16);
            for i in 0..self.outputs {
                v[i] = f64::from(o16[i]) / 65535.0;
            }
        }
        for (x, c) in v.iter_mut().zip(&self.m_curves) {
            *x = c.eval(*x);
        }
        if let Some(m) = &self.matrix
            && self.outputs == 3
        {
            let (a, b, c) = (v[0], v[1], v[2]);
            for i in 0..3 {
                v[i] = m[i * 3] * a + m[i * 3 + 1] * b + m[i * 3 + 2] * c + m[9 + i];
            }
        }
        for (x, c) in v.iter_mut().zip(&self.b_curves) {
            *x = c.eval(*x);
        }
        out[..self.outputs].copy_from_slice(&v[..self.outputs]);
    }
}
