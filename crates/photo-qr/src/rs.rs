//! Reed–Solomon over GF(256) with polynomial 0x11D (QR codes, b = 0).

pub(crate) struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
}

pub(crate) fn gf() -> &'static Gf {
    static G: std::sync::OnceLock<Gf> = std::sync::OnceLock::new();
    G.get_or_init(|| {
        let mut g = Gf { exp: [0; 512], log: [0; 256] };
        let mut x: u16 = 1;
        for i in 0..255 {
            g.exp[i] = x as u8;
            g.log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11D;
            }
        }
        for i in 255..512 {
            g.exp[i] = g.exp[i - 255];
        }
        g
    })
}

impl Gf {
    #[inline]
    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 { 0 } else { self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize] }
    }
    #[inline]
    fn inv(&self, a: u8) -> u8 {
        self.exp[255 - self.log[a as usize] as usize]
    }
    #[inline]
    fn pow_alpha(&self, e: usize) -> u8 {
        self.exp[e % 255]
    }
}

/// Correct `block` (data followed by `npar` parity bytes) in place.
/// Returns false if the errors exceed the correction capacity.
pub(crate) fn correct(block: &mut [u8], npar: usize) -> bool {
    let g = gf();
    let n = block.len();
    if npar == 0 || npar >= n {
        return npar == 0;
    }
    // Syndromes S_i = r(alpha^i), i = 0..npar-1.
    let syndromes = |b: &[u8]| -> Vec<u8> {
        (0..npar)
            .map(|i| {
                let x = g.pow_alpha(i);
                b.iter().fold(0u8, |acc, &c| g.mul(acc, x) ^ c)
            })
            .collect()
    };
    let s = syndromes(block);
    if s.iter().all(|&v| v == 0) {
        return true;
    }
    // Berlekamp–Massey.
    let mut sigma = vec![0u8; npar + 1];
    let mut prev = vec![0u8; npar + 1];
    sigma[0] = 1;
    prev[0] = 1;
    let (mut l, mut m, mut b) = (0usize, 1usize, 1u8);
    for k in 0..npar {
        let mut d = s[k];
        for i in 1..=l {
            d ^= g.mul(sigma[i], s[k - i]);
        }
        if d == 0 {
            m += 1;
            continue;
        }
        let coef = g.mul(d, g.inv(b));
        let t = sigma.clone();
        for i in m..=npar {
            sigma[i] ^= g.mul(coef, prev[i - m]);
        }
        if 2 * l <= k {
            l = k + 1 - l;
            prev = t;
            b = d;
            m = 1;
        } else {
            m += 1;
        }
    }
    if l == 0 || 2 * l > npar {
        return false;
    }
    // Error evaluator omega = S(x) * sigma(x) mod x^npar.
    let mut omega = vec![0u8; npar];
    for i in 0..npar {
        let mut v = 0;
        for j in 0..=i.min(l) {
            v ^= g.mul(sigma[j], s[i - j]);
        }
        omega[i] = v;
    }
    let eval = |p: &[u8], x: u8| p.iter().rev().fold(0u8, |acc, &c| g.mul(acc, x) ^ c);
    // Chien search over positions; position p (0 = last byte) has X = alpha^p.
    let mut found = 0;
    for p in 0..n {
        let xinv = g.pow_alpha(255 - (p % 255));
        if eval(&sigma[..=l], xinv) != 0 {
            continue;
        }
        // Forney with b = 0: e = X * omega(X^-1) / sigma'(X^-1).
        let mut deriv = 0u8;
        let mut xp = 1u8;
        for i in (1..=l).step_by(2) {
            // sigma'(x) = sum over odd i of sigma_i x^(i-1)
            deriv ^= g.mul(sigma[i], xp);
            xp = g.mul(xp, g.mul(xinv, xinv));
        }
        if deriv == 0 {
            return false;
        }
        let x = g.pow_alpha(p);
        let e = g.mul(x, g.mul(eval(&omega, xinv), g.inv(deriv)));
        block[n - 1 - p] ^= e;
        found += 1;
    }
    if found != l {
        return false;
    }
    syndromes(block).iter().all(|&v| v == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(data: &[u8], npar: usize) -> Vec<u8> {
        let g = gf();
        // Generator: prod (x - alpha^i), i = 0..npar-1.
        let mut generator = vec![1u8];
        for i in 0..npar {
            let mut next = vec![0u8; generator.len() + 1];
            for (j, &c) in generator.iter().enumerate() {
                next[j] ^= c;
                next[j + 1] ^= g.mul(c, g.pow_alpha(i));
            }
            generator = next;
        }
        let mut rem = data.to_vec();
        rem.extend(std::iter::repeat_n(0, npar));
        for i in 0..data.len() {
            let c = rem[i];
            if c != 0 {
                for (j, &gc) in generator.iter().enumerate() {
                    rem[i + j] ^= g.mul(c, gc);
                }
            }
        }
        let mut out = data.to_vec();
        out.extend_from_slice(&rem[data.len()..]);
        out
    }

    #[test]
    fn corrects_up_to_capacity_and_rejects_beyond() {
        let data: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(37) ^ 0x5A).collect();
        for npar in [7, 10, 18, 30] {
            let clean = encode(&data, npar);
            let mut ok = clean.clone();
            assert!(correct(&mut ok, npar));
            assert_eq!(ok, clean);
            for errors in 1..=npar / 2 {
                let mut bad = clean.clone();
                for e in 0..errors {
                    let pos = (e * 13 + 5) % bad.len();
                    bad[pos] ^= (e as u8).wrapping_mul(29) | 1;
                }
                assert!(correct(&mut bad, npar), "npar {npar} errors {errors}");
                assert_eq!(bad, clean);
            }
        }
    }
}
