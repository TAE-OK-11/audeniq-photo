//! Port of Tesseract's LSTM network (`src/lstm`): deserialization and the
//! inference-only forward pass for the layer types the released models use
//! (Input, Convolve, Maxpool, Series, Reversed, FullyConnected, LSTM).

pub(crate) mod io;
pub(crate) mod rand;
mod simd;
mod tables;

use crate::reader::Reader;
use crate::{Error, Result};
use io::{HEIGHT, NetIo, WIDTH};
use rand::TRand;
use tables::{LOGISTIC_TABLE, TANH_TABLE};

const NF_LAYER_SPECIFIC_LR: i32 = 64;
const STATE_CLIP: f32 = 100.0;

// NetworkType values.
const NT_NONE: i8 = 0;
const NT_INPUT: i8 = 1;
const NT_CONVOLVE: i8 = 2;
const NT_MAXPOOL: i8 = 3;
const NT_PARALLEL: i8 = 4;
const NT_SERIES: i8 = 9;
const NT_RECONFIG: i8 = 10;
const NT_XREVERSED: i8 = 11;
const NT_YREVERSED: i8 = 12;
const NT_XYTRANSPOSE: i8 = 13;
const NT_LSTM: i8 = 14;
const NT_LSTM_SUMMARY: i8 = 15;
const NT_LOGISTIC: i8 = 16;
const NT_POSCLIP: i8 = 17;
const NT_SYMCLIP: i8 = 18;
const NT_TANH: i8 = 19;
const NT_RELU: i8 = 20;
const NT_LINEAR: i8 = 21;
const NT_SOFTMAX: i8 = 22;
const NT_SOFTMAX_NO_CTC: i8 = 23;

const TYPE_NAMES: [&str; 27] = [
    "Invalid",
    "Input",
    "Convolve",
    "Maxpool",
    "Parallel",
    "Replicated",
    "ParBidiLSTM",
    "DepParUDLSTM",
    "Par2dLSTM",
    "Series",
    "Reconfig",
    "RTLReversed",
    "TTBReversed",
    "XYTranspose",
    "LSTM",
    "SummLSTM",
    "Logistic",
    "LinLogistic",
    "LinTanh",
    "Tanh",
    "Relu",
    "Linear",
    "Softmax",
    "SoftmaxNoCTC",
    "LSTMSoftmax",
    "LSTMBinarySoftmax",
    "TensorFlow",
];

/// Tesseract `Tanh()` (table interpolation, `TFloat = float`), written
/// branch-free (odd symmetry by sign select) so slices of it vectorize;
/// results are bit-identical to the recursive original.
#[inline(always)]
pub(crate) fn tanh(x: f32) -> f32 {
    let y = x.abs() * 256.0;
    let index = y as u32;
    let i = index.min(4094) as usize;
    let (t0, t1) = (TANH_TABLE[i], TANH_TABLE[i + 1]);
    let r = if index >= 4095 {
        1.0
    } else {
        t0 + (t1 - t0) * (y - index as f32)
    };
    if x < 0.0 { -r } else { r }
}

/// Tesseract `Logistic()` (`1 - Logistic(-x)` for negative x), branch-free.
#[inline(always)]
pub(crate) fn logistic(x: f32) -> f32 {
    let y = x.abs() * 256.0;
    let index = y as u32;
    let i = index.min(4094) as usize;
    let (l0, l1) = (LOGISTIC_TABLE[i], LOGISTIC_TABLE[i + 1]);
    let r = if index >= 4095 {
        1.0
    } else {
        l0 + (l1 - l0) * (y - index as f32)
    };
    if x < 0.0 { 1.0 - r } else { r }
}

photo_core::multiversion! {
    fn tanh_slice(v: &mut [f32]) -> () = tanh_slice_body;
}

#[inline(always)]
fn tanh_slice_body(v: &mut [f32]) {
    v.iter_mut().for_each(|x| *x = tanh(*x));
}

photo_core::multiversion! {
    fn logistic_slice(v: &mut [f32]) -> () = logistic_slice_body;
}

#[inline(always)]
fn logistic_slice_body(v: &mut [f32]) {
    v.iter_mut().for_each(|x| *x = logistic(*x));
}

photo_core::multiversion! {
    /// One LSTM cell update from the four gate pre-activations
    /// (CI=0, GI=1, GF1=2, GO=3), in Tesseract's operation order.
    fn lstm_cell(lines: &mut [Vec<f32>; 4], state: &mut [f32], output: &mut [f32]) -> () = lstm_cell_body;
}

#[inline(always)]
fn lstm_cell_body(lines: &mut [Vec<f32>; 4], state: &mut [f32], output: &mut [f32]) {
    tanh_slice_body(&mut lines[0]);
    for line in &mut lines[1..] {
        logistic_slice_body(line);
    }
    let ns = state.len();
    let (ci, gi, gf, go) = (
        &lines[0][..ns],
        &lines[1][..ns],
        &lines[2][..ns],
        &lines[3][..ns],
    );
    for i in 0..ns {
        state[i] *= gf[i];
    }
    for i in 0..ns {
        state[i] += ci[i] * gi[i];
    }
    for s in state.iter_mut() {
        *s = s.clamp(-STATE_CLIP, STATE_CLIP);
    }
    for i in 0..ns {
        output[i] = tanh(state[i]) * go[i];
    }
}

fn softmax_in_place(v: &mut [f32]) {
    if v.is_empty() {
        return;
    }
    let max = v
        .iter()
        .copied()
        .fold(v[0], |m, x| if x > m { x } else { m });
    let mut total = 0.0f32;
    for x in v.iter_mut() {
        let p = (*x - max).clamp(-86.0, 0.0).exp();
        total += p;
        *x = p;
    }
    if total > 0.0 {
        for x in v.iter_mut() {
            *x /= total;
        }
    }
}

/// `WeightMatrix`: int8 rows with per-row scales, or float rows; the last
/// column of each row is the bias.
#[derive(Debug)]
pub(crate) enum Weights {
    Int {
        rows: usize,
        cols: usize,
        /// Rows in blocks of eight, columns interleaved in groups of
        /// `group` (see [`shape_int`]), so eight row sums accumulate side
        /// by side.
        shaped: Vec<i8>,
        /// 2 (portable / AVX2 `vpmaddwd`) or 4 (AVX-VNNI `vpdpbusd`).
        group: usize,
        /// Per-row constant added to the dot product: the bias column
        /// times 127, minus `128 * sum(row)` for the VNNI layout (whose
        /// kernel feeds `u + 128` as unsigned bytes).
        bias: Vec<i32>,
        scales: Vec<f32>,
    },
    Float {
        rows: usize,
        cols: usize,
        w: Vec<f32>,
    },
}

impl Weights {
    fn read(r: &mut Reader<'_>) -> Result<Weights> {
        let mode = r.u8()?;
        if mode & 128 == 0 {
            return Err(Error::Model("old weight format unsupported"));
        }
        let (rows, cols) = (r.i32()?, r.i32()?);
        if !(0..=65535).contains(&rows) || !(0..=65535).contains(&cols) {
            return Err(Error::Model("bad weight dimensions"));
        }
        let (rows, cols) = (rows as usize, cols as usize);
        if mode & 1 != 0 {
            let _empty = r.i8()?;
            let w: Vec<i8> = r.take(rows * cols)?.iter().map(|&b| b as i8).collect();
            let n = r.u32()? as usize;
            if n != rows {
                return Err(Error::Model("scale count mismatch"));
            }
            let mut scales = Vec::with_capacity(n);
            for _ in 0..n {
                scales.push((r.f64()? / 127.0) as f32);
            }
            let group = if simd::vnni() { 4 } else { 2 };
            let (shaped, bias) = shape_int(rows, cols, &w, group);
            Ok(Weights::Int {
                rows,
                cols,
                shaped,
                group,
                bias,
                scales,
            })
        } else {
            let _empty = r.f64()?;
            let mut w = Vec::with_capacity(rows * cols);
            for _ in 0..rows * cols {
                w.push(r.f64()? as f32);
            }
            Ok(Weights::Float { rows, cols, w })
        }
    }

    fn rows(&self) -> usize {
        match self {
            Weights::Int { rows, .. } | Weights::Float { rows, .. } => *rows,
        }
    }

    /// `MatrixDotVector` for int8 inputs (exact integer sums, one float
    /// multiply, as both the generic and SIMD Tesseract kernels compute).
    fn dot_int(&self, u: &[i8], v: &mut [f32]) {
        let Weights::Int {
            rows,
            cols,
            shaped,
            group,
            bias,
            scales,
        } = self
        else {
            unreachable!("int input to float weights")
        };
        if *group == 4 {
            simd::dot_int_rows_vnni(*rows, *cols - 1, shaped, bias, scales, u, v);
        } else {
            simd::dot_int_rows(*rows, *cols - 1, shaped, bias, scales, u, v);
        }
    }

    fn dot_float(&self, u: &[f32], v: &mut [f32]) {
        let Weights::Float { rows, cols, w } = self else {
            unreachable!("float input to int weights")
        };
        let ni = cols - 1;
        for i in 0..*rows {
            let wi = &w[i * cols..(i + 1) * cols];
            let mut total = 0.0f32;
            for j in 0..ni {
                total += wi[j] * u[j];
            }
            v[i] = total + wi[ni];
        }
    }

    /// Dot product of the current time step of `input` into `v`.
    fn dot(&self, input: &NetIo, t: usize, v: &mut [f32]) {
        if input.int_mode {
            self.dot_int(input.irow(t), v);
        } else {
            self.dot_float(input.frow(t), v);
        }
    }
}

/// Reorder int8 weights (`rows x cols`, last column the bias) for the
/// kernels: rows in blocks of eight (the last block zero-padded) and, per
/// block, columns in groups of `group` stored row after row
/// (`[r0c0, r0c1, .., r1c0, ..]`, 8 * `group` bytes, zero past the last
/// column). As Tesseract's `IntSimdMatrix` shaping: the kernels then
/// accumulate eight rows in eight lanes and never reduce horizontally.
/// Returns the per-row constant term too (see `Weights::Int::bias`).
fn shape_int(rows: usize, cols: usize, w: &[i8], group: usize) -> (Vec<i8>, Vec<i32>) {
    let ni = cols.saturating_sub(1);
    let ngroups = ni.div_ceil(group);
    let blocks = rows.div_ceil(8);
    let mut shaped = vec![0i8; blocks * ngroups * 8 * group];
    for row in 0..rows {
        let (blk, r) = (row / 8, row % 8);
        for col in 0..ni {
            let (k, e) = (col / group, col % group);
            shaped[(blk * ngroups + k) * 8 * group + group * r + e] = w[row * cols + col];
        }
    }
    let bias = (0..rows)
        .map(|row| {
            let b = i32::from(w[row * cols + ni]) * 127;
            if group == 4 {
                let sum: i32 = w[row * cols..row * cols + ni]
                    .iter()
                    .map(|&x| i32::from(x))
                    .sum();
                b - 128 * sum
            } else {
                b
            }
        })
        .collect();
    (shaped, bias)
}

/// `MatrixDotVector` over int8 inputs with [`shape_int`] weights: exact
/// integer sums, then one float multiply per row (as Tesseract's generic
/// and SIMD kernels compute). Compiled once generically and once for AVX2.
#[inline(always)]
pub(crate) fn dot_int_rows_body(
    rows: usize,
    ni: usize,
    shaped: &[i8],
    bias: &[i32],
    scales: &[f32],
    u: &[i8],
    v: &mut [f32],
) {
    let npairs = ni.div_ceil(2);
    if npairs == 0 {
        for row in 0..rows {
            v[row] = bias[row] as f32 * scales[row];
        }
        return;
    }
    let full = ni / 2;
    let u = &u[..ni];
    for (blk, wb) in shaped.chunks_exact(npairs * 16).enumerate() {
        let mut acc = [0i32; 8];
        for k in 0..full {
            let (u0, u1) = (i32::from(u[2 * k]), i32::from(u[2 * k + 1]));
            let b: &[i8; 16] = wb[k * 16..k * 16 + 16].try_into().expect("16");
            for r in 0..8 {
                acc[r] += i32::from(b[2 * r]) * u0 + i32::from(b[2 * r + 1]) * u1;
            }
        }
        if npairs > full {
            let u0 = i32::from(u[2 * full]);
            let b = &wb[full * 16..full * 16 + 16];
            for r in 0..8 {
                acc[r] += i32::from(b[2 * r]) * u0;
            }
        }
        for r in 0..8 {
            let row = blk * 8 + r;
            if row < rows {
                v[row] = (acc[r] + bias[row]) as f32 * scales[row];
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    Logistic,
    PosClip,
    SymClip,
    Tanh,
    Relu,
    Linear,
    Softmax,
}

#[derive(Debug)]
pub(crate) enum Layer {
    Input {
        height: i32,
        depth: i32,
    },
    Convolve {
        ni: usize,
        half_x: i32,
        half_y: i32,
    },
    Maxpool {
        ni: usize,
        x_scale: i32,
        y_scale: i32,
    },
    Series(Vec<Layer>),
    XReversed(Box<Layer>),
    YReversed(Box<Layer>),
    XYTranspose(Box<Layer>),
    Full {
        act: Act,
        no: usize,
        w: Weights,
    },
    Lstm {
        summary: bool,
        ni: usize,
        ns: usize,
        na: usize,
        gates: Box<[Weights; 4]>,
    },
}

/// Header fields of a serialized layer.
struct Header {
    ni: i32,
    no: i32,
}

pub(crate) struct Network {
    pub(crate) root: Layer,
    /// `NumInputs()`: the input image height.
    pub(crate) num_inputs: i32,
    pub(crate) input_height: i32,
    pub(crate) input_depth: i32,
    pub(crate) num_outputs: usize,
}

impl Network {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Network> {
        let (root, h) = read_layer(r, 0)?;
        let (height, depth) = input_shape(&root).ok_or(Error::Model("network has no input"))?;
        Ok(Network {
            root,
            num_inputs: h.ni,
            input_height: height,
            input_depth: depth,
            num_outputs: h.no as usize,
        })
    }

    /// Product of the x reductions (`XScaleFactor`).
    pub(crate) fn x_scale(&self) -> i32 {
        x_scale(&self.root)
    }

    pub(crate) fn forward(&self, input: &NetIo, rand: &mut TRand) -> NetIo {
        forward(&self.root, input, rand)
    }
}

fn input_shape(l: &Layer) -> Option<(i32, i32)> {
    match l {
        Layer::Input { height, depth } => Some((*height, *depth)),
        Layer::Series(v) => v.first().and_then(input_shape),
        _ => None,
    }
}

fn x_scale(l: &Layer) -> i32 {
    match l {
        Layer::Series(v) => v.iter().map(x_scale).product(),
        Layer::Maxpool { x_scale, .. } => *x_scale,
        Layer::XReversed(b) | Layer::YReversed(b) => x_scale(b),
        _ => 1,
    }
}

fn read_layer(r: &mut Reader<'_>, depth: usize) -> Result<(Layer, Header)> {
    if depth > 32 {
        return Err(Error::Model("network nested too deeply"));
    }
    let mut kind = r.i8()?;
    if kind == NT_NONE {
        let name = r.string()?;
        kind = TYPE_NAMES
            .iter()
            .position(|n| *n == name)
            .ok_or(Error::Model("unknown layer type"))? as i8;
    }
    let _training = r.i8()?;
    let _backprop = r.i8()?;
    let flags = r.i32()?;
    let ni = r.i32()?;
    let no = r.i32()?;
    let _num_weights = r.i32()?;
    let _name = r.string()?;
    let h = Header { ni, no };
    if ni < 0 || no < 0 {
        return Err(Error::Model("bad layer size"));
    }
    let layer = match kind {
        NT_INPUT => {
            let _batch = r.i32()?;
            let height = r.i32()?;
            let _width = r.i32()?;
            let depth = r.i32()?;
            let _loss = r.i32()?;
            Layer::Input { height, depth }
        }
        NT_CONVOLVE => {
            let (half_x, half_y) = (r.i32()?, r.i32()?);
            if !(0..=16).contains(&half_x) || !(0..=16).contains(&half_y) {
                return Err(Error::Model("bad convolution size"));
            }
            Layer::Convolve {
                ni: ni as usize,
                half_x,
                half_y,
            }
        }
        NT_MAXPOOL => {
            let (x_scale, y_scale) = (r.i32()?, r.i32()?);
            if !(1..=16).contains(&x_scale) || !(1..=16).contains(&y_scale) {
                return Err(Error::Model("bad maxpool size"));
            }
            Layer::Maxpool {
                ni: ni as usize,
                x_scale,
                y_scale,
            }
        }
        NT_SERIES | NT_XREVERSED | NT_YREVERSED | NT_XYTRANSPOSE => {
            let n = r.u32()? as usize;
            if n == 0 || n > 64 {
                return Err(Error::Model("bad layer stack"));
            }
            let mut stack = Vec::with_capacity(n);
            for _ in 0..n {
                stack.push(read_layer(r, depth + 1)?.0);
            }
            if flags & NF_LAYER_SPECIFIC_LR != 0 {
                let k = r.u32()? as usize;
                r.take(k * 4)?;
            }
            match kind {
                NT_SERIES => Layer::Series(stack),
                _ if stack.len() != 1 => {
                    return Err(Error::Model("reversed layer needs one child"));
                }
                NT_XREVERSED => Layer::XReversed(Box::new(stack.remove(0))),
                NT_YREVERSED => Layer::YReversed(Box::new(stack.remove(0))),
                _ => Layer::XYTranspose(Box::new(stack.remove(0))),
            }
        }
        NT_LOGISTIC | NT_POSCLIP | NT_SYMCLIP | NT_TANH | NT_RELU | NT_LINEAR | NT_SOFTMAX
        | NT_SOFTMAX_NO_CTC => {
            let act = match kind {
                NT_LOGISTIC => Act::Logistic,
                NT_POSCLIP => Act::PosClip,
                NT_SYMCLIP => Act::SymClip,
                NT_TANH => Act::Tanh,
                NT_RELU => Act::Relu,
                NT_LINEAR => Act::Linear,
                _ => Act::Softmax,
            };
            let w = Weights::read(r)?;
            if w.rows() != no as usize {
                return Err(Error::Model("fully connected size mismatch"));
            }
            Layer::Full {
                act,
                no: no as usize,
                w,
            }
        }
        NT_LSTM | NT_LSTM_SUMMARY => {
            let na = r.i32()?;
            let gates = [
                Weights::read(r)?,
                Weights::read(r)?,
                Weights::read(r)?,
                Weights::read(r)?,
            ];
            let ns = gates[0].rows();
            if na as usize != ni as usize + ns {
                return Err(Error::Model("2-D LSTM unsupported"));
            }
            if gates.iter().any(|g| g.rows() != ns) {
                return Err(Error::Model("LSTM gate size mismatch"));
            }
            Layer::Lstm {
                summary: kind == NT_LSTM_SUMMARY,
                ni: ni as usize,
                ns,
                na: na as usize,
                gates: Box::new(gates),
            }
        }
        NT_RECONFIG | NT_PARALLEL => return Err(Error::Model("unsupported layer type")),
        _ => return Err(Error::Model("unsupported layer type")),
    };
    Ok((layer, h))
}

fn forward(layer: &Layer, input: &NetIo, rand: &mut TRand) -> NetIo {
    let mut out = NetIo::default();
    match layer {
        Layer::Input { .. } => out = input.clone(),
        Layer::Series(stack) => {
            let mut cur = forward(&stack[0], input, rand);
            for l in &stack[1..] {
                cur = forward(l, &cur, rand);
            }
            out = cur;
        }
        Layer::XReversed(inner) => {
            let mut rev = NetIo::default();
            rev.copy_with_x_reversal(input);
            let res = forward(inner, &rev, rand);
            out.copy_with_x_reversal(&res);
        }
        Layer::YReversed(_) => unreachable!("rejected at load"),
        Layer::XYTranspose(inner) => {
            let mut rev = NetIo::default();
            rev.copy_with_xy_transpose(input);
            let res = forward(inner, &rev, rand);
            out.copy_with_xy_transpose(&res);
        }
        Layer::Convolve { ni, half_x, half_y } => {
            let (ni, hx, hy) = (*ni, *half_x, *half_y);
            let y_scale = (2 * hy + 1) as usize;
            let no = ni * (2 * hx as usize + 1) * y_scale;
            out.resize_like(input, no);
            let m = out.map.clone();
            let mut dest = m.first();
            loop {
                let t = dest.t();
                let mut out_ix = 0;
                for x in -hx..=hx {
                    let mut xi = dest;
                    if !xi.add_offset(&m, x, WIDTH) {
                        out.randomize(t, out_ix, y_scale * ni, rand);
                    } else {
                        let mut out_iy = out_ix;
                        for y in -hy..=hy {
                            let mut yi = xi;
                            if !yi.add_offset(&m, y, HEIGHT) {
                                out.randomize(t, out_iy, ni, rand);
                            } else {
                                out.copy_step_general(t, out_iy, ni, input, yi.t(), 0);
                            }
                            out_iy += ni;
                        }
                    }
                    out_ix += y_scale * ni;
                }
                if !dest.increment(&m) {
                    break;
                }
            }
        }
        Layer::Maxpool {
            ni,
            x_scale,
            y_scale,
        } => {
            let mut map = input.map.clone();
            map.scale_xy(*x_scale, *y_scale);
            out.resize_to_map(input.int_mode, map, *ni);
            let (om, im) = (out.map.clone(), input.map.clone());
            let mut dest = om.first();
            if om.width() > 0 {
                loop {
                    let out_t = dest.t();
                    let src = im.at(
                        dest.idx[0],
                        dest.idx[HEIGHT] * y_scale,
                        dest.idx[WIDTH] * x_scale,
                    );
                    out.copy_step_from(out_t, input, src.t());
                    for x in 0..*x_scale {
                        for y in 0..*y_scale {
                            let mut s = src;
                            if s.add_offset(&im, x, WIDTH) && s.add_offset(&im, y, HEIGHT) {
                                out.maxpool_step(out_t, input, s.t());
                            }
                        }
                    }
                    if !dest.increment(&om) {
                        break;
                    }
                }
            }
        }
        Layer::Full { act, no, w } => {
            if *act == Act::Softmax {
                out.resize_float_like(input, *no);
            } else {
                out.resize_like(input, *no);
            }
            let mut line = vec![0.0f32; *no];
            for t in 0..input.width() {
                w.dot(input, t, &mut line);
                apply(*act, &mut line);
                out.write_step(t, &line);
            }
        }
        Layer::Lstm {
            summary,
            ni,
            ns,
            na,
            gates,
        } => {
            lstm_forward(*summary, *ni, *ns, *na, gates, input, &mut out);
        }
    }
    out
}

fn apply(act: Act, v: &mut [f32]) {
    match act {
        Act::Tanh => tanh_slice(v),
        Act::Logistic => logistic_slice(v),
        Act::PosClip => v.iter_mut().for_each(|x| *x = x.clamp(0.0, 1.0)),
        Act::SymClip => v.iter_mut().for_each(|x| *x = x.clamp(-1.0, 1.0)),
        Act::Relu => v.iter_mut().for_each(|x| {
            if *x <= 0.0 {
                *x = 0.0
            }
        }),
        Act::Softmax => softmax_in_place(v),
        Act::Linear => {}
    }
}

fn lstm_forward(
    summary: bool,
    ni: usize,
    ns: usize,
    na: usize,
    gates: &[Weights; 4],
    input: &NetIo,
    out: &mut NetIo,
) {
    if summary {
        let mut map = input.map.clone();
        map.reduce_width_to_1();
        out.resize_to_map(input.int_mode, map, ns);
    } else {
        out.resize_like(input, ns);
    }
    let mut source = NetIo::default();
    source.resize_like(input, na);
    let mut lines: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0f32; ns]);
    let mut state = vec![0.0f32; ns];
    let mut output = vec![0.0f32; ns];
    let (im, om) = (input.map.clone(), out.map.clone());
    if im.width() == 0 {
        return;
    }
    let mut src = im.first();
    let mut dest = om.first();
    loop {
        let t = src.t();
        source.copy_step_general(t, 0, ni, input, t, 0);
        source.write_step_part(t, ni, &output);
        for (g, line) in gates.iter().zip(lines.iter_mut()) {
            g.dot(&source, t, line);
        }
        lstm_cell(&mut lines, &mut state, &mut output);
        if summary {
            if src.is_last(&im, WIDTH) {
                out.write_step(dest.t(), &output);
                dest.increment(&om);
            }
        } else {
            out.write_step(t, &output);
        }
        if src.is_last(&im, WIDTH) {
            state.iter_mut().for_each(|x| *x = 0.0);
            output.iter_mut().for_each(|x| *x = 0.0);
        }
        if !src.increment(&im) {
            break;
        }
    }
}
