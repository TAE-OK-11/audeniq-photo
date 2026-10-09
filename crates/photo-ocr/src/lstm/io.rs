//! `StrideMap` and `NetworkIO`: the 2-D (batch, y, x) × features buffers
//! that flow between layers, in int8 or float mode.

use super::rand::TRand;

pub(crate) const BATCH: usize = 0;
pub(crate) const HEIGHT: usize = 1;
pub(crate) const WIDTH: usize = 2;

#[derive(Clone, Debug, Default)]
pub(crate) struct StrideMap {
    shape: [i32; 3],
    incr: [i32; 3],
    heights: Vec<i32>,
    widths: Vec<i32>,
}

impl StrideMap {
    pub(crate) fn set_stride(&mut self, pairs: &[(i32, i32)]) {
        let (mut max_h, mut max_w) = (0, 0);
        for &(h, w) in pairs {
            self.heights.push(h);
            self.widths.push(w);
            max_h = max_h.max(h);
            max_w = max_w.max(w);
        }
        self.shape = [self.heights.len() as i32, max_h, max_w];
        self.compute_incr();
    }

    pub(crate) fn scale_xy(&mut self, x: i32, y: i32) {
        for h in &mut self.heights {
            *h /= y;
        }
        for w in &mut self.widths {
            *w /= x;
        }
        self.shape[HEIGHT] /= y;
        self.shape[WIDTH] /= x;
        self.compute_incr();
    }

    pub(crate) fn reduce_width_to_1(&mut self) {
        self.widths.iter_mut().for_each(|w| *w = 1);
        self.shape[WIDTH] = 1;
        self.compute_incr();
    }

    pub(crate) fn transpose_xy(&mut self) {
        self.shape.swap(HEIGHT, WIDTH);
        std::mem::swap(&mut self.heights, &mut self.widths);
        self.compute_incr();
    }

    fn compute_incr(&mut self) {
        self.incr[2] = 1;
        for d in (0..2).rev() {
            self.incr[d] = self.incr[d + 1] * self.shape[d + 1];
        }
    }

    pub(crate) fn size(&self, d: usize) -> i32 {
        self.shape[d]
    }

    pub(crate) fn width(&self) -> usize {
        (self.incr[BATCH] * self.shape[BATCH]).max(0) as usize
    }

    pub(crate) fn first(&self) -> Index {
        Index { t: 0, idx: [0; 3] }
    }

    pub(crate) fn at(&self, b: i32, y: i32, x: i32) -> Index {
        let mut i = Index {
            t: 0,
            idx: [b, y, x],
        };
        i.set_t(self);
        i
    }
}

/// `StrideMap::Index` (the map is passed explicitly).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Index {
    pub(crate) t: i32,
    pub(crate) idx: [i32; 3],
}

impl Index {
    pub(crate) fn t(&self) -> usize {
        self.t as usize
    }

    pub(crate) fn max_index(&self, m: &StrideMap, dim: usize) -> i32 {
        let max = m.shape[dim] - 1;
        if dim == BATCH {
            return max;
        }
        let b = self.idx[BATCH] as usize;
        let v = if dim == HEIGHT { &m.heights } else { &m.widths };
        match v.get(b) {
            Some(&s) if s <= max => s - 1,
            _ => max,
        }
    }

    fn is_valid(&self, m: &StrideMap) -> bool {
        self.idx.iter().all(|&i| i >= 0) && (0..3).all(|d| self.idx[d] <= self.max_index(m, d))
    }

    pub(crate) fn is_last(&self, m: &StrideMap, dim: usize) -> bool {
        self.max_index(m, dim) == self.idx[dim]
    }

    pub(crate) fn add_offset(&mut self, m: &StrideMap, offset: i32, dim: usize) -> bool {
        self.idx[dim] += offset;
        self.set_t(m);
        self.is_valid(m)
    }

    pub(crate) fn increment(&mut self, m: &StrideMap) -> bool {
        for d in (0..3).rev() {
            if !self.is_last(m, d) {
                self.t += m.incr[d];
                self.idx[d] += 1;
                return true;
            }
            self.t -= m.incr[d] * self.idx[d];
            self.idx[d] = 0;
        }
        false
    }

    fn set_t(&mut self, m: &StrideMap) {
        self.t = (0..3).map(|d| m.incr[d] * self.idx[d]).sum();
    }
}

/// `NetworkIO`: rows of `nf` features per time step `t`.
#[derive(Clone, Debug, Default)]
pub(crate) struct NetIo {
    pub(crate) int_mode: bool,
    pub(crate) nf: usize,
    pub(crate) i: Vec<i8>,
    pub(crate) f: Vec<f32>,
    pub(crate) map: StrideMap,
}

/// Tesseract `IntCastRounded(double)`.
pub(crate) fn round_f64(x: f64) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        -((-x + 0.5) as i32)
    }
}

/// Tesseract `IntCastRounded(float)`.
pub(crate) fn round_f32(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        -((-x + 0.5) as i32)
    }
}

photo_core::multiversion! {
    /// `round_f32(x * 127)` clamped to int8, over a slice.
    fn quantize(dst: &mut [i8], src: &[f32]) -> () = quantize_body;
}

#[inline(always)]
fn quantize_body(dst: &mut [i8], src: &[f32]) {
    for (d, &x) in dst.iter_mut().zip(src) {
        // round_f32 without the branch: identical for every input
        // (NaN and -0.0 give 0; huge values saturate, then clamp).
        let x = x * 127.0;
        let m = (x.abs() + 0.5) as i32;
        let r = if x >= 0.0 { m } else { -m };
        *d = r.clamp(-127, 127) as i8;
    }
}

impl NetIo {
    pub(crate) fn resize_to_map(&mut self, int_mode: bool, map: StrideMap, nf: usize) {
        let n = map.width() * nf;
        self.int_mode = int_mode;
        self.nf = nf;
        self.map = map;
        if int_mode {
            self.f.clear();
            self.i.clear();
            self.i.resize(n, 0);
        } else {
            self.i.clear();
            self.f.clear();
            self.f.resize(n, 0.0);
        }
    }

    pub(crate) fn resize_like(&mut self, src: &NetIo, nf: usize) {
        self.resize_to_map(src.int_mode, src.map.clone(), nf);
    }

    pub(crate) fn resize_float_like(&mut self, src: &NetIo, nf: usize) {
        self.resize_to_map(false, src.map.clone(), nf);
    }

    pub(crate) fn width(&self) -> usize {
        self.map.width()
    }

    pub(crate) fn irow(&self, t: usize) -> &[i8] {
        &self.i[t * self.nf..(t + 1) * self.nf]
    }

    pub(crate) fn frow(&self, t: usize) -> &[f32] {
        &self.f[t * self.nf..(t + 1) * self.nf]
    }

    pub(crate) fn copy_step_general(
        &mut self,
        dest_t: usize,
        dest_off: usize,
        n: usize,
        src: &NetIo,
        src_t: usize,
        src_off: usize,
    ) {
        if self.int_mode {
            let d = dest_t * self.nf + dest_off;
            let s = src_t * src.nf + src_off;
            self.i[d..d + n].copy_from_slice(&src.i[s..s + n]);
        } else {
            let d = dest_t * self.nf + dest_off;
            let s = src_t * src.nf + src_off;
            self.f[d..d + n].copy_from_slice(&src.f[s..s + n]);
        }
    }

    pub(crate) fn copy_step_from(&mut self, dest_t: usize, src: &NetIo, src_t: usize) {
        let n = self.nf;
        self.copy_step_general(dest_t, 0, n, src, src_t, 0);
    }

    pub(crate) fn randomize(&mut self, t: usize, off: usize, n: usize, rand: &mut TRand) {
        let base = t * self.nf + off;
        if self.int_mode {
            for v in &mut self.i[base..base + n] {
                *v = round_f64(rand.signed_rand(127.0)) as i8;
            }
        } else {
            for v in &mut self.f[base..base + n] {
                *v = rand.signed_rand(1.0) as f32;
            }
        }
    }

    pub(crate) fn write_step_part(&mut self, t: usize, off: usize, input: &[f32]) {
        let base = t * self.nf + off;
        if self.int_mode {
            quantize(&mut self.i[base..base + input.len()], input);
        } else {
            self.f[base..base + input.len()].copy_from_slice(input);
        }
    }

    pub(crate) fn write_step(&mut self, t: usize, input: &[f32]) {
        let n = self.nf;
        self.write_step_part(t, 0, &input[..n]);
    }

    pub(crate) fn maxpool_step(&mut self, dest_t: usize, src: &NetIo, src_t: usize) {
        let n = self.nf;
        if self.int_mode {
            let (d, s) = (
                &mut self.i[dest_t * n..(dest_t + 1) * n],
                &src.i[src_t * n..(src_t + 1) * n],
            );
            for (a, &b) in d.iter_mut().zip(s) {
                *a = (*a).max(b);
            }
        } else {
            let (d, s) = (
                &mut self.f[dest_t * n..(dest_t + 1) * n],
                &src.f[src_t * n..(src_t + 1) * n],
            );
            for (a, &b) in d.iter_mut().zip(s) {
                if *a < b {
                    *a = b;
                }
            }
        }
    }

    pub(crate) fn copy_with_x_reversal(&mut self, src: &NetIo) {
        self.resize_like(src, src.nf);
        let m = &src.map;
        let mut b = m.first();
        loop {
            let mut y = b;
            loop {
                let mut fwd = y;
                let mut rev = y;
                let last = rev.max_index(m, WIDTH);
                rev.add_offset(m, last, WIDTH);
                loop {
                    self.copy_step_from(rev.t(), src, fwd.t());
                    if !(fwd.add_offset(m, 1, WIDTH) && rev.add_offset(m, -1, WIDTH)) {
                        break;
                    }
                }
                if !y.add_offset(m, 1, HEIGHT) {
                    break;
                }
            }
            if !b.add_offset(m, 1, BATCH) {
                break;
            }
        }
    }

    pub(crate) fn copy_with_xy_transpose(&mut self, src: &NetIo) {
        let mut map = src.map.clone();
        map.transpose_xy();
        self.resize_to_map(src.int_mode, map, src.nf);
        let (sm, dm) = (src.map.clone(), self.map.clone());
        let mut sb = sm.first();
        let mut db = dm.first();
        loop {
            let mut sy = sb;
            let mut dx = db;
            loop {
                let mut sx = sy;
                let mut dy = dx;
                loop {
                    self.copy_step_from(dy.t(), src, sx.t());
                    if !(sx.add_offset(&sm, 1, WIDTH) && dy.add_offset(&dm, 1, HEIGHT)) {
                        break;
                    }
                }
                if !(sy.add_offset(&sm, 1, HEIGHT) && dx.add_offset(&dm, 1, WIDTH)) {
                    break;
                }
            }
            if !(sb.add_offset(&sm, 1, BATCH) && db.add_offset(&dm, 1, BATCH)) {
                break;
            }
        }
    }
}
