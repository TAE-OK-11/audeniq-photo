//! Register code for PostScript calculator functions.
//!
//! Shadings evaluate their function once per device pixel, so a stack
//! interpreter spends most of its time on `dup`/`exch`/`index`/`roll` and on
//! pushing literals. When the stack layout at every point of a program is
//! known at compile time (stack-operator operands are literals, as in
//! virtually every real program), it is tracked there instead:
//!
//! - every stack slot becomes a register (or a known constant), so stack
//!   operators only rename registers and cost nothing at run time;
//! - operators whose operands are all constants are evaluated once, with the
//!   same arithmetic as the interpreter ([`Un::apply`], [`Bin::apply`]);
//! - `if`/`ifelse` become jumps. Where both arms leave the same depth and
//!   operand types, register moves join them; where they do not (some
//!   programs push a different number of results on one path), each arm
//!   gets its own copy of the rest of the program and its own result list.
//!
//! Anything else (computed stack-operator operands, over- or underflow,
//! oversized code) is left to the interpreter, which keeps its exact
//! semantics, errors included.

use super::{Bin, PostScriptOp, Un};
use crate::function::Values;

/// Registers available to compiled code (a power of two: indices are masked).
const REGS: usize = 256;
/// The interpreter's stack size; deeper programs are not compiled.
const STACK: usize = 64;
/// Instructions per program (bounds the copies made for unbalanced arms).
const MAX_CODE: usize = 4096;
/// Operators compiled in all, retries included (nested unbalanced
/// conditionals are compiled once per enclosing level that tries to join).
const BUDGET: usize = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Float,
    /// Stored as 1.0 / 0.0, which is what the interpreter's `as_f32` and
    /// `as_bool` make of a boolean, so only `not`, `or` and `xor` care.
    Bool,
}

/// A stack slot at compile time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Slot {
    ty: Ty,
    val: Val,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Val {
    Reg(u16),
    Const(f32),
}

#[derive(Debug, Clone, Copy)]
enum Code {
    Un(Un),
    Bin(Bin),
    NotF,
    NotB,
    OrF,
    OrB,
    XorF,
    XorB,
    Mov,
    /// `a` is the condition; jump to `d` when it is false.
    JumpIfFalse,
    /// Jump to `d`.
    Jump,
    /// Stop; the result is result list `d`.
    Exit,
}

#[derive(Debug, Clone, Copy)]
struct Ins {
    code: Code,
    d: u16,
    a: u16,
    b: u16,
}

/// A compiled program for a fixed number of inputs.
#[derive(Debug)]
pub(crate) struct Compiled {
    inputs: usize,
    /// Initial values of registers `inputs..inputs + consts.len()`.
    consts: Vec<f32>,
    regs: usize,
    code: Vec<Ins>,
    /// Registers holding the final stack (bottom first) of each exit.
    results: Vec<Vec<u16>>,
}

struct Compiler {
    inputs: usize,
    consts: Vec<f32>,
    temps: usize,
    code: Vec<Ins>,
    stack: Vec<Slot>,
    results: Vec<Vec<Slot>>,
    /// Operators left to compile before giving up.
    budget: usize,
}

/// Temporaries are numbered from here during compilation (renumbered after
/// the constants once their count is known).
const TEMP: u16 = 1 << 15;

/// What is left to compile: the rest of each enclosing procedure, innermost
/// last.
type Frames<'p> = Vec<&'p [PostScriptOp]>;

fn next<'p>(frames: &mut Frames<'p>) -> Option<&'p PostScriptOp> {
    loop {
        let top = frames.last_mut()?;
        if let Some((op, rest)) = top.split_first() {
            *top = rest;
            return Some(op);
        }
        frames.pop();
    }
}

impl Compiled {
    /// Compile `program` for `inputs` input values, or `None` if its stack
    /// layout is not static.
    pub(crate) fn new(program: &[PostScriptOp], inputs: usize) -> Option<Self> {
        if inputs > STACK {
            return None;
        }
        let mut c = Compiler {
            inputs,
            consts: Vec::new(),
            temps: 0,
            code: Vec::new(),
            stack: (0..inputs)
                .map(|i| Slot {
                    ty: Ty::Float,
                    val: Val::Reg(i as u16),
                })
                .collect(),
            results: Vec::new(),
            budget: BUDGET,
        };
        if c.block(&mut vec![program])? {
            c.exit()?;
        }
        let results = std::mem::take(&mut c.results)
            .into_iter()
            .map(|r| r.iter().map(|s| c.reg(s.val)).collect::<Option<Vec<u16>>>())
            .collect::<Option<Vec<_>>>()?;
        let regs = c.inputs + c.consts.len() + c.temps;
        if regs > REGS {
            return None;
        }
        // Temporaries were numbered from `TEMP`; move them past the constants.
        let base = (c.inputs + c.consts.len()) as u16;
        let fix = |r: u16| if r >= TEMP { r - TEMP + base } else { r };
        let mut code = c.code;
        for ins in &mut code {
            match ins.code {
                Code::Jump | Code::Exit => {}
                Code::JumpIfFalse => ins.a = fix(ins.a),
                _ => {
                    ins.d = fix(ins.d);
                    ins.a = fix(ins.a);
                    ins.b = fix(ins.b);
                }
            }
        }
        Some(Self {
            inputs,
            consts: c.consts,
            regs,
            code,
            results: results
                .into_iter()
                .map(|r| r.into_iter().map(fix).collect())
                .collect(),
        })
    }

    /// Run with `input` (already clamped). `None` when the input count is
    /// not the one compiled for; the caller interprets instead.
    pub(crate) fn run(&self, input: &[f32]) -> Option<Values> {
        if input.len() != self.inputs {
            return None;
        }
        Some(if self.regs <= 32 {
            self.run_in::<32>(input)
        } else if self.regs <= 64 {
            self.run_in::<64>(input)
        } else {
            self.run_in::<REGS>(input)
        })
    }

    #[inline(always)]
    fn run_in<const N: usize>(&self, input: &[f32]) -> Values {
        let mut r = [0f32; N];
        r[..self.inputs].copy_from_slice(input);
        r[self.inputs..self.inputs + self.consts.len()].copy_from_slice(&self.consts);
        // Register numbers are below `regs <= N` (checked at compile time);
        // the mask only lets the bounds checks go.
        let m = |i: u16| usize::from(i) & (N - 1);
        let mut pc = 0;
        // Every path ends in an `Exit`.
        while let Some(&ins) = self.code.get(pc) {
            pc += 1;
            let (a, b) = (r[m(ins.a)], r[m(ins.b)]);
            let v = match ins.code {
                Code::Un(u) => u.apply(a),
                Code::Bin(o) => o.apply(a, b),
                Code::NotF => !(a as i32) as f32,
                Code::NotB => bool_f(a == 0.0),
                Code::OrF => ((a as i32) | (b as i32)) as f32,
                Code::OrB => bool_f(a != 0.0 || b != 0.0),
                Code::XorF => ((a as i32) ^ (b as i32)) as f32,
                Code::XorB => bool_f((a != 0.0) ^ (b != 0.0)),
                Code::Mov => a,
                Code::JumpIfFalse => {
                    if a == 0.0 {
                        pc = usize::from(ins.d);
                    }
                    continue;
                }
                Code::Jump => {
                    pc = usize::from(ins.d);
                    continue;
                }
                Code::Exit => {
                    return self.results[usize::from(ins.d)]
                        .iter()
                        .map(|&o| r[m(o)])
                        .collect();
                }
            };
            r[m(ins.d)] = v;
        }
        Values::new()
    }
}

fn bool_f(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

impl Compiler {
    fn pop(&mut self) -> Option<Slot> {
        self.stack.pop()
    }

    fn push(&mut self, s: Slot) -> Option<()> {
        // The interpreter drops (or fails on) pushes past its stack size.
        if self.stack.len() >= STACK {
            return None;
        }
        self.stack.push(s);
        Some(())
    }

    /// A constant operand of a stack operator, as the interpreter reads it.
    fn pop_const(&mut self) -> Option<f32> {
        match self.pop()?.val {
            Val::Const(c) => Some(c),
            Val::Reg(_) => None,
        }
    }

    fn temp(&mut self) -> Option<u16> {
        if self.temps >= REGS {
            return None;
        }
        self.temps += 1;
        Some(TEMP + (self.temps - 1) as u16)
    }

    /// The register holding `v` (constants get one, deduplicated by bits).
    fn reg(&mut self, v: Val) -> Option<u16> {
        match v {
            Val::Reg(r) => Some(r),
            Val::Const(c) => {
                let bits = c.to_bits();
                let i = match self.consts.iter().position(|k| k.to_bits() == bits) {
                    Some(i) => i,
                    None => {
                        if self.consts.len() >= REGS {
                            return None;
                        }
                        self.consts.push(c);
                        self.consts.len() - 1
                    }
                };
                Some((self.inputs + i) as u16)
            }
        }
    }

    fn ins(&mut self, code: Code, d: u16, a: u16, b: u16) -> Option<()> {
        if self.code.len() >= MAX_CODE {
            return None;
        }
        self.code.push(Ins { code, d, a, b });
        Some(())
    }

    /// End this path with the current stack as its result.
    fn exit(&mut self) -> Option<()> {
        let i = u16::try_from(self.results.len()).ok()?;
        self.results.push(self.stack.clone());
        self.ins(Code::Exit, i, 0, 0)
    }

    /// Emit a one-operand operator (constant-folded when possible).
    fn unary(&mut self, ty: Ty, a: Slot, code: Code, fold: impl Fn(f32) -> f32) -> Option<()> {
        let val = match a.val {
            Val::Const(x) => Val::Const(fold(x)),
            Val::Reg(ra) => {
                let d = self.temp()?;
                self.ins(code, d, ra, 0)?;
                Val::Reg(d)
            }
        };
        self.push(Slot { ty, val })
    }

    /// Emit a two-operand operator (constant-folded when possible).
    fn binary(
        &mut self,
        ty: Ty,
        a: Slot,
        b: Slot,
        code: Code,
        fold: Option<&dyn Fn(f32, f32) -> f32>,
    ) -> Option<()> {
        let val = match (a.val, b.val, fold) {
            (Val::Const(x), Val::Const(y), Some(f)) => Val::Const(f(x, y)),
            _ => {
                let (ra, rb) = (self.reg(a.val)?, self.reg(b.val)?);
                let d = self.temp()?;
                self.ins(code, d, ra, rb)?;
                Val::Reg(d)
            }
        };
        self.push(Slot { ty, val })
    }

    /// Compile until `frames` run out. `Some(true)`: the stack is live (the
    /// caller continues or exits); `Some(false)`: every path already exited.
    fn block<'p>(&mut self, frames: &mut Frames<'p>) -> Option<bool> {
        while let Some(op) = next(frames) {
            self.budget = self.budget.checked_sub(1)?;
            match op {
                PostScriptOp::Number(n) => self.push(Slot {
                    ty: Ty::Float,
                    val: Val::Const(n.as_f64() as f32),
                })?,
                PostScriptOp::True | PostScriptOp::False => self.push(Slot {
                    ty: Ty::Bool,
                    val: Val::Const(bool_f(matches!(op, PostScriptOp::True))),
                })?,
                PostScriptOp::Un(u) => {
                    let a = self.pop()?;
                    self.unary(Ty::Float, a, Code::Un(*u), |x| u.apply(x))?;
                }
                PostScriptOp::Bin(o) => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    // Integer division (by zero) and shifts can panic; keep
                    // that at run time, where the interpreter would hit it.
                    let fold = |x: f32, y: f32| o.apply(x, y);
                    let fold: Option<&dyn Fn(f32, f32) -> f32> =
                        (!matches!(o, Bin::Idiv | Bin::Bitshift)).then_some(&fold);
                    self.binary(Ty::Float, a, b, Code::Bin(*o), fold)?;
                }
                PostScriptOp::Not => {
                    let a = self.pop()?;
                    match a.ty {
                        Ty::Float => {
                            self.unary(Ty::Float, a, Code::NotF, |x| !(x as i32) as f32)?
                        }
                        Ty::Bool => self.unary(Ty::Bool, a, Code::NotB, |x| bool_f(x == 0.0))?,
                    }
                }
                PostScriptOp::Or | PostScriptOp::Xor => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    let or = matches!(op, PostScriptOp::Or);
                    if a.ty == Ty::Float && b.ty == Ty::Float {
                        let f = |x: f32, y: f32| {
                            if or {
                                ((x as i32) | (y as i32)) as f32
                            } else {
                                ((x as i32) ^ (y as i32)) as f32
                            }
                        };
                        let code = if or { Code::OrF } else { Code::XorF };
                        self.binary(Ty::Float, a, b, code, Some(&f))?;
                    } else {
                        let f = |x: f32, y: f32| {
                            if or {
                                bool_f(x != 0.0 || y != 0.0)
                            } else {
                                bool_f((x != 0.0) ^ (y != 0.0))
                            }
                        };
                        let code = if or { Code::OrB } else { Code::XorB };
                        self.binary(Ty::Bool, a, b, code, Some(&f))?;
                    }
                }
                PostScriptOp::If(p) | PostScriptOp::IfElse(p, _) => {
                    let otherwise: &'p [PostScriptOp] = match op {
                        PostScriptOp::IfElse(_, q) => q,
                        _ => &[],
                    };
                    let cond = self.pop()?;
                    match cond.val {
                        // Known condition: only that arm.
                        Val::Const(c) => frames.push(if c != 0.0 { p } else { otherwise }),
                        Val::Reg(rc) => {
                            if !self.conditional(rc, p, otherwise, frames)? {
                                return Some(false);
                            }
                        }
                    }
                }
                PostScriptOp::Copy => {
                    let n = self.pop_const()? as u32 as usize;
                    let start = self.stack.len().checked_sub(n)?;
                    for i in start..self.stack.len() {
                        let s = self.stack[i];
                        self.push(s)?;
                    }
                }
                PostScriptOp::Dup => {
                    let s = *self.stack.last()?;
                    self.push(s)?;
                }
                PostScriptOp::Exch => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(b)?;
                    self.push(a)?;
                }
                PostScriptOp::Index => {
                    let n = self.pop_const()? as u32 as usize;
                    let i = self.stack.len().checked_sub(n + 1)?;
                    let s = self.stack[i];
                    self.push(s)?;
                }
                PostScriptOp::Pop => {
                    self.pop()?;
                }
                PostScriptOp::Roll => {
                    let j = self.pop_const()? as i32;
                    let n = self.pop_const()? as u32 as usize;
                    let trimmed = self.stack.len().checked_sub(n)?;
                    let target = &mut self.stack[trimmed..];
                    if target.is_empty() {
                        continue;
                    }
                    if j >= 0 {
                        let shift = j as usize % target.len();
                        target.rotate_right(shift);
                    } else {
                        let shift = (-j) as usize % target.len();
                        target.rotate_left(shift);
                    }
                }
            }
        }
        Some(true)
    }

    /// A conditional on register `rc`. Joins the arms when they leave the
    /// same layout; otherwise compiles the rest of the program (`frames`)
    /// into each arm, consuming it, and returns `Some(false)`.
    fn conditional<'p>(
        &mut self,
        rc: u16,
        taken: &'p [PostScriptOp],
        otherwise: &'p [PostScriptOp],
        frames: &mut Frames<'p>,
    ) -> Option<bool> {
        let before = self.stack.clone();
        let mark = (
            self.code.len(),
            self.results.len(),
            self.temps,
            self.consts.len(),
        );
        if self.join(rc, taken, otherwise).is_some() {
            return Some(true);
        }
        // Not joinable (or an arm exited inside): forget the attempt and
        // split. Nothing that remains refers to what it allocated.
        self.code.truncate(mark.0);
        self.results.truncate(mark.1);
        self.temps = mark.2;
        self.consts.truncate(mark.3);
        let rest = std::mem::take(frames);
        let jump = self.code.len();
        self.ins(Code::JumpIfFalse, 0, rc, 0)?;
        for (i, arm) in [taken, otherwise].into_iter().enumerate() {
            if i == 1 {
                self.code[jump].d = u16::try_from(self.code.len()).ok()?;
            }
            self.stack = before.clone();
            let mut f = rest.clone();
            f.push(arm);
            if self.block(&mut f)? {
                self.exit()?;
            }
        }
        Some(false)
    }

    /// Compile both arms to completion and join their stacks with register
    /// moves. `None` if they differ in depth or operand type.
    fn join(&mut self, rc: u16, taken: &[PostScriptOp], otherwise: &[PostScriptOp]) -> Option<()> {
        let before = self.stack.clone();
        let jump = self.code.len();
        self.ins(Code::JumpIfFalse, 0, rc, 0)?;
        if !self.block(&mut vec![taken])? {
            return None;
        }
        let taken_stack = std::mem::replace(&mut self.stack, before);
        // The taken arm's moves come after its code, which is only known
        // once the other arm is compiled: set the taken code aside.
        let taken_code = self.code.split_off(jump + 1);
        if !self.block(&mut vec![otherwise])? {
            return None;
        }
        let other_code = self.code.split_off(jump + 1);
        let other_stack = std::mem::take(&mut self.stack);
        if taken_stack.len() != other_stack.len() {
            return None;
        }
        let mut merged = Vec::with_capacity(taken_stack.len());
        let mut moves = Vec::new();
        for (t, o) in taken_stack.iter().zip(&other_stack) {
            if t.ty != o.ty {
                return None;
            }
            if t.val == o.val {
                merged.push(*t);
                continue;
            }
            let d = self.temp()?;
            moves.push((d, self.reg(t.val)?, self.reg(o.val)?));
            merged.push(Slot {
                ty: t.ty,
                val: Val::Reg(d),
            });
        }
        // JumpIfFalse → else; taken; moves; Jump → end; else: other; moves.
        self.code.extend(taken_code);
        for &(d, t, _) in &moves {
            self.ins(Code::Mov, d, t, 0)?;
        }
        let skip = self.code.len();
        self.ins(Code::Jump, 0, 0, 0)?;
        let else_at = self.code.len();
        // The other arm was compiled at `jump + 1`; move its jump targets.
        let shift = else_at - (jump + 1);
        for mut ins in other_code {
            if matches!(ins.code, Code::Jump | Code::JumpIfFalse) {
                ins.d += shift as u16;
            }
            self.ins(ins.code, ins.d, ins.a, ins.b)?;
        }
        for &(d, _, o) in &moves {
            self.ins(Code::Mov, d, o, 0)?;
        }
        self.code[jump].d = else_at as u16;
        self.code[skip].d = u16::try_from(self.code.len()).ok()?;
        self.stack = merged;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse_procedure;
    use super::Compiled;

    /// Results of the register code and the interpreter, bit for bit.
    fn both(src: &str, input: &[f32]) -> (Vec<u32>, Vec<u32>) {
        let prog = parse_procedure(src.as_bytes()).unwrap();
        let c = Compiled::new(&prog, input.len()).expect(src);
        let compiled = c.run(input).unwrap();
        let mut stack = super::super::InterpreterStack::new();
        for &v in input {
            let _ = stack.push(super::super::Argument::Float(v));
        }
        super::super::eval_inner(&prog, &mut stack).unwrap();
        let bits = |v: &mut dyn Iterator<Item = f32>| v.map(f32::to_bits).collect::<Vec<_>>();
        (
            bits(&mut compiled.into_iter()),
            bits(&mut stack.items().iter().map(|a| a.as_f32())),
        )
    }

    #[test]
    fn agrees_with_interpreter() {
        // Joined arms, unbalanced arms (split), stack operators, booleans.
        let progs = [
            "{ dup 0.5 gt { 1 sub } { 2 mul } ifelse exch }",
            "{ dup 2.0 le { pop 1 2 3 } { dup 6 le { 0.5 mul 0 1 } { 7 8 9 } ifelse } ifelse }",
            "{ dup 0.3 lt { 0 0 0 } if add }",
            "{ 2 copy gt { exch } if 2 copy mul 3 1 roll add 1 index exch div }",
            "{ 1 index 0.2 gt exch 0.7 lt and { 1 } { 0 } ifelse }",
            "{ dup 0.5 lt exch 0.1 gt xor not { 1 } if }",
            "{ 100 sub exch 100 sub atan 30 div floor 2 mod 0 gt { 0 1 } { 1 0 } ifelse }",
        ];
        for p in progs {
            for x in [0.0f32, 0.05, 0.25, 0.5, 0.75, 1.0, 3.0, 7.5] {
                for y in [0.0f32, 0.4, 1.0] {
                    let (c, i) = both(p, &[x, y]);
                    assert_eq!(c, i, "{p} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn deeply_nested_unbalanced_conditionals_stay_cheap() {
        // Each level retries its inner levels; the budget stops it.
        let mut src = String::from("{ 0 1 2 }");
        for _ in 0..40 {
            src = format!("{{ dup 0.5 lt {src} {{ 1 }} ifelse }}");
        }
        let prog = parse_procedure(src.as_bytes()).unwrap();
        let t = std::time::Instant::now();
        let _ = Compiled::new(&prog, 1);
        assert!(t.elapsed().as_secs() < 2);
    }

    #[test]
    fn computed_stack_operands_are_not_compiled() {
        let prog = parse_procedure(b"{ dup 0.5 gt { 1 } { 2 } ifelse index }").unwrap();
        assert!(Compiled::new(&prog, 2).is_none());
    }
}
