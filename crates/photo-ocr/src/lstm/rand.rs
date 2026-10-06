//! `TRand`: `std::minstd_rand` as libstdc++ implements it.

const M: u64 = 2_147_483_647;

#[derive(Clone, Debug)]
pub(crate) struct TRand {
    x: u64,
}

impl Default for TRand {
    fn default() -> Self {
        TRand { x: 1 }
    }
}

impl TRand {
    pub(crate) fn set_seed(&mut self, seed: u64) {
        let s = seed % M;
        self.x = if s == 0 { 1 } else { s };
    }

    pub(crate) fn int_rand(&mut self) -> i32 {
        self.x = self.x * 48271 % M;
        self.x as i32
    }

    pub(crate) fn signed_rand(&mut self, range: f64) -> f64 {
        range * 2.0 * f64::from(self.int_rand()) / f64::from(i32::MAX) - range
    }
}
