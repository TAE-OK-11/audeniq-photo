#![deny(unsafe_code)]
//! CRC-32 and Adler-32 (zlib-rs kernels: PCLMUL/AVX-512/NEON when present).

#[derive(Debug, Clone, Copy, Default)]
pub struct Crc32(u32);

impl Crc32 {
    pub const fn new() -> Self {
        Crc32(0)
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0 = crate::engine::crc32::crc32(self.0, data);
    }
    pub fn finish(&self) -> u32 {
        self.0
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    crate::engine::crc32::crc32(0, data)
}

#[derive(Debug, Clone, Copy)]
pub struct Adler32(u32);

impl Default for Adler32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Adler32 {
    pub const fn new() -> Self {
        Adler32(1)
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0 = crate::engine::adler32::adler32(self.0, data);
    }
    pub fn finish(&self) -> u32 {
        self.0
    }
}

pub fn adler32(data: &[u8]) -> u32 {
    crate::engine::adler32::adler32(1, data)
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
