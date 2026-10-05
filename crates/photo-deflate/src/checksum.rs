//! CRC-32 (slicing-by-8, as in zlib's `crc32_little`) and Adler-32.

const fn crc_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][n] = c;
        n += 1;
    }
    let mut n = 0;
    while n < 256 {
        let mut k = 1;
        while k < 8 {
            let prev = t[k - 1][n];
            t[k][n] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            k += 1;
        }
        n += 1;
    }
    t
}

static CRC: [[u32; 256]; 8] = crc_tables();

#[derive(Debug, Clone, Copy)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Crc32(0xFFFF_FFFF)
    }
    pub fn update(&mut self, mut data: &[u8]) {
        let mut c = self.0;
        while data.len() >= 8 {
            let lo = c ^ u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let hi = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            c = CRC[7][(lo & 0xFF) as usize]
                ^ CRC[6][((lo >> 8) & 0xFF) as usize]
                ^ CRC[5][((lo >> 16) & 0xFF) as usize]
                ^ CRC[4][(lo >> 24) as usize]
                ^ CRC[3][(hi & 0xFF) as usize]
                ^ CRC[2][((hi >> 8) & 0xFF) as usize]
                ^ CRC[1][((hi >> 16) & 0xFF) as usize]
                ^ CRC[0][(hi >> 24) as usize];
            data = &data[8..];
        }
        for &b in data {
            c = CRC[0][((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }
    pub fn finish(&self) -> u32 {
        !self.0
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

#[derive(Debug, Clone, Copy)]
pub struct Adler32 {
    a: u32,
    b: u32,
}

impl Default for Adler32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Adler32 {
    pub const fn new() -> Self {
        Adler32 { a: 1, b: 0 }
    }
    pub fn update(&mut self, data: &[u8]) {
        // NMAX from zlib: the largest n with 255n(n+1)/2 + (n+1)(BASE-1) < 2^32.
        const NMAX: usize = 5552;
        const BASE: u32 = 65521;
        let (mut a, mut b) = (self.a, self.b);
        for chunk in data.chunks(NMAX) {
            for &x in chunk {
                a += u32::from(x);
                b += a;
            }
            a %= BASE;
            b %= BASE;
        }
        self.a = a;
        self.b = b;
    }
    pub fn finish(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

pub fn adler32(data: &[u8]) -> u32 {
    let mut a = Adler32::new();
    a.update(data);
    a.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let big = vec![0xFFu8; 100_000];
        let mut split = Adler32::new();
        split.update(&big[..33_333]);
        split.update(&big[33_333..]);
        assert_eq!(split.finish(), adler32(&big));
        let mut c = Crc32::new();
        c.update(&big[..7]);
        c.update(&big[7..]);
        assert_eq!(c.finish(), crc32(&big));
    }
}
