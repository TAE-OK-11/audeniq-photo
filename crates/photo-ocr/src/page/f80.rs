//! x87 80-bit extended precision (`long double` on x86-64 GCC) in
//! software: 64-bit significand, round-to-nearest-even. Tesseract's
//! quadratic least squares accumulates and divides in this format.

use std::cmp::Ordering;

/// A finite extended-precision value (no NaN/infinity/denormals: the
/// callers only ever see moderate finite numbers).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct F80 {
    neg: bool,
    /// Value = mant * 2^(exp - 63); `mant` has its top bit set unless zero.
    exp: i32,
    mant: u64,
}

impl F80 {
    pub const ZERO: F80 = F80 {
        neg: false,
        exp: 0,
        mant: 0,
    };

    pub fn is_zero(self) -> bool {
        self.mant == 0
    }

    /// Normalises and rounds a 128-bit significand `m * 2^(e - 127)`.
    fn round(neg: bool, mut e: i32, m: u128, sticky_in: bool) -> F80 {
        if m == 0 {
            return F80 { neg, ..F80::ZERO };
        }
        let lz = m.leading_zeros() as i32;
        let m = m << lz;
        e -= lz;
        // m now has bit 127 set; keep top 64 bits.
        let hi = (m >> 64) as u64;
        let lo = m as u64;
        let half = 1u64 << 63;
        let round_up =
            lo > half || (lo == half && (sticky_in || hi & 1 == 1)) || (lo == half && sticky_in);
        let (mut mant, mut exp) = (hi, e);
        if round_up {
            let (r, carry) = mant.overflowing_add(1);
            mant = r;
            if carry {
                mant = 1 << 63;
                exp += 1;
            }
        }
        F80 { neg, exp, mant }
    }

    pub fn from_f64(v: f64) -> F80 {
        if v == 0.0 {
            return F80 {
                neg: v.is_sign_negative(),
                ..F80::ZERO
            };
        }
        let bits = v.to_bits();
        let neg = bits >> 63 == 1;
        let be = ((bits >> 52) & 0x7ff) as i32;
        let frac = bits & ((1 << 52) - 1);
        let (m, e) = if be == 0 {
            (frac, -1074)
        } else {
            (frac | (1 << 52), be - 1075)
        };
        // value = m * 2^e
        F80::round(neg, e + 127, u128::from(m), false)
    }

    pub fn from_i64(v: i64) -> F80 {
        F80::round(v < 0, 127, u128::from(v.unsigned_abs()), false)
    }

    /// Rounds to the nearest `f64` (ties to even).
    pub fn to_f64(self) -> f64 {
        if self.mant == 0 {
            return if self.neg { -0.0 } else { 0.0 };
        }
        // value = mant * 2^(exp-63); keep 53 bits.
        let mut m = self.mant >> 11;
        let rest = self.mant & 0x7ff;
        let half = 0x400;
        if rest > half || (rest == half && m & 1 == 1) {
            m += 1;
        }
        let mut e = self.exp - 52;
        if m == 1 << 53 {
            m >>= 1;
            e += 1;
        }
        let v = (m as f64) * 2f64.powi(e);
        if self.neg { -v } else { v }
    }

    fn negate(self) -> F80 {
        F80 {
            neg: !self.neg,
            ..self
        }
    }

    fn plus(self, o: F80) -> F80 {
        if self.mant == 0 {
            // IEEE: +0 + -0 is +0.
            if o.mant == 0 {
                return F80 {
                    neg: self.neg && o.neg,
                    ..F80::ZERO
                };
            }
            return o;
        }
        if o.mant == 0 {
            return self;
        }
        let (a, b) = if self.exp > o.exp || (self.exp == o.exp && self.mant >= o.mant) {
            (self, o)
        } else {
            (o, self)
        };
        // Align b to a with 64 guard bits.
        let am = u128::from(a.mant) << 64;
        let shift = (a.exp - b.exp) as u32;
        let (bm, sticky) = if shift >= 128 {
            (0u128, true)
        } else {
            let full = u128::from(b.mant) << 64;
            let s = full >> shift;
            let lost = shift > 0 && (full & ((1u128 << shift) - 1)) != 0;
            (s, lost)
        };
        // a.exp corresponds to bit 127 of am (value = am * 2^(a.exp-127)).
        if a.neg == b.neg {
            let (sum, carry) = am.overflowing_add(bm);
            if carry {
                let s = (sum >> 1) | (1u128 << 127);
                let st = sticky || sum & 1 == 1;
                return F80::round(a.neg, a.exp + 1, s, st);
            }
            F80::round(a.neg, a.exp, sum, sticky)
        } else {
            let mut diff = am - bm;
            if sticky {
                // The true subtrahend is slightly larger than bm.
                diff -= 1;
            }
            if diff == 0 {
                // An exact cancellation is +0 when rounding to nearest.
                return F80::ZERO;
            }
            F80::round(a.neg, a.exp, diff, sticky)
        }
    }

    fn minus(self, o: F80) -> F80 {
        self.plus(o.negate())
    }

    fn times(self, o: F80) -> F80 {
        if self.mant == 0 || o.mant == 0 {
            return F80 {
                neg: self.neg != o.neg,
                ..F80::ZERO
            };
        }
        let p = u128::from(self.mant) * u128::from(o.mant);
        // value = p * 2^(e1-63 + e2-63) = p * 2^((e1+e2+1) - 127)
        F80::round(self.neg != o.neg, self.exp + o.exp + 1, p, false)
    }

    fn over(self, o: F80) -> F80 {
        if self.mant == 0 {
            return F80 {
                neg: self.neg != o.neg,
                ..F80::ZERO
            };
        }
        let num = u128::from(self.mant) << 64;
        let den = u128::from(o.mant);
        let q = num / den;
        let r = num % den;
        // Two more quotient words for the guard bits.
        let extra = (r << 64) / den;
        let r2 = (r << 64) % den;
        let sh = q.leading_zeros(); // 63 or 64
        let full = (q << sh) | (extra >> (64 - sh));
        let lost = (extra & ((1u128 << (64 - sh)) - 1)) != 0 || r2 != 0;
        F80::round(
            self.neg != o.neg,
            self.exp - o.exp + 63 - sh as i32 + 1 - 1,
            full,
            lost,
        )
    }

    pub fn compare(self, o: F80) -> Ordering {
        let d = self.minus(o);
        if d.mant == 0 {
            Ordering::Equal
        } else if d.neg {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }

    pub fn lt(self, o: F80) -> bool {
        self.compare(o) == Ordering::Less
    }

    pub fn ge(self, o: F80) -> bool {
        self.compare(o) != Ordering::Less
    }
}

macro_rules! f80_op {
    ($tr:ident, $m:ident, $f:ident) => {
        impl std::ops::$tr for F80 {
            type Output = F80;
            fn $m(self, o: F80) -> F80 {
                self.$f(o)
            }
        }
    };
}
f80_op!(Add, add, plus);
f80_op!(Sub, sub, minus);
f80_op!(Mul, mul, times);
f80_op!(Div, div, over);

impl std::ops::Neg for F80 {
    type Output = F80;
    fn neg(self) -> F80 {
        self.negate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::{Add, Div, Mul, Sub};

    #[test]
    fn basic_ops() {
        let a = F80::from_f64(3.0);
        let b = F80::from_f64(7.0);
        assert_eq!(a.mul(b).to_f64(), 21.0);
        assert_eq!(b.sub(a).to_f64(), 4.0);
        assert_eq!(a.sub(b).to_f64(), -4.0);
        assert_eq!(F80::from_f64(1.0).div(a).to_f64(), 1.0 / 3.0);
        assert_eq!(F80::from_i64(-5).add(F80::from_f64(0.5)).to_f64(), -4.5);
        // 2^64 + 1 is not representable: rounds to even.
        let big = F80::from_f64(2f64.powi(64));
        assert_eq!(big.add(F80::from_f64(1.0)).sub(big).to_f64(), 0.0);
        assert_eq!(big.add(F80::from_f64(3.0)).sub(big).to_f64(), 4.0);
        let z = F80::from_f64(0.0);
        assert!(z.sub(z).to_f64().is_sign_positive());
        assert!(a.sub(a).to_f64().is_sign_positive());
        assert!(
            F80::from_f64(-0.0)
                .add(F80::from_f64(-0.0))
                .to_f64()
                .is_sign_negative()
        );
    }
}

#[cfg(test)]
mod tests2 {
    use super::*;
    use std::ops::{Add, Div};

    #[test]
    fn x87_case() {
        let xv = F80::from_f64(f64::from_bits(0)).add(F80::from_i64(0xd9f146 << 3)); // 0xd.9f146p+19
        let cv = F80::from_i64(-(0xb323da << 3));
        let q = cv.div(xv);
        assert_eq!(q.mant, 0xd26bfc3b02ec1401, "{:x} {}", q.mant, q.exp);
        assert_eq!(q.to_f64(), -0.821_960_224_539_836_6);
    }
}
