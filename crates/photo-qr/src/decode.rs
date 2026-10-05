//! Grid → codewords → Reed–Solomon → payload validation (quirc decode.c,
//! with the standard block interleaving of ISO/IEC 18004 §7.6).

use crate::identify::Code;
use crate::rs;
use crate::tables::VERSIONS;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    InvalidGridSize,
    Format,
    Ecc,
    Payload,
}

fn mask_bit(mask: u8, i: usize, j: usize) -> bool {
    match mask {
        0 => (i + j).is_multiple_of(2),
        1 => i.is_multiple_of(2),
        2 => j.is_multiple_of(3),
        3 => (i + j).is_multiple_of(3),
        4 => ((i / 2) + (j / 3)).is_multiple_of(2),
        5 => (i * j) % 2 + (i * j) % 3 == 0,
        6 => ((i * j) % 2 + (i * j) % 3).is_multiple_of(2),
        _ => ((i * j) % 3 + (i + j) % 2).is_multiple_of(2),
    }
}

fn reserved(version: usize, i: usize, j: usize) -> bool {
    let size = version * 4 + 17;
    if (i < 9 && j < 9) || (i + 8 >= size && j < 9) || (i < 9 && j + 8 >= size) || i == 6 || j == 6
    {
        return true;
    }
    if version >= 7 && ((i < 6 && j + 11 >= size) || (i + 11 >= size && j < 6)) {
        return true;
    }
    let ap = VERSIONS[version - 1].apat;
    let mut ai = None;
    let mut aj = None;
    for (k, &p) in ap.iter().enumerate() {
        if (p as usize).abs_diff(i) < 3 {
            ai = Some(k);
        }
        if (p as usize).abs_diff(j) < 3 {
            aj = Some(k);
        }
    }
    if let (Some(ai), Some(aj)) = (ai, aj) {
        let last = ap.len() - 1;
        if (ai > 0 && ai < last) || (aj > 0 && aj < last) || (ai == last && aj == last) {
            return true;
        }
    }
    false
}

/// BCH(15,5) format words: nearest valid codeword within distance 3.
fn correct_format(raw: u16) -> Option<u16> {
    let mut best: Option<(u32, u16)> = None;
    for d in 0u16..32 {
        let mut rem = d << 10;
        for bit in (10..15).rev() {
            if rem & (1 << bit) != 0 {
                rem ^= 0x537 << (bit - 10);
            }
        }
        let word = (d << 10) | rem;
        let dist = (word ^ raw).count_ones();
        if best.is_none_or(|b| dist < b.0) {
            best = Some((dist, word));
        }
    }
    best.filter(|b| b.0 <= 3).map(|b| b.1)
}

fn read_format(code: &Code, which: bool) -> Option<(u8, u8)> {
    let n = code.size;
    let mut f: u16 = 0;
    if which {
        for i in 0..7 {
            f = (f << 1) | u16::from(code.bit(8, n - 1 - i));
        }
        for i in 0..8 {
            f = (f << 1) | u16::from(code.bit(n - 8 + i, 8));
        }
    } else {
        let xs = [8, 8, 8, 8, 8, 8, 8, 8, 7, 5, 4, 3, 2, 1, 0];
        let ys = [0, 1, 2, 3, 4, 5, 7, 8, 8, 8, 8, 8, 8, 8, 8];
        for i in (0..15).rev() {
            f = (f << 1) | u16::from(code.bit(xs[i], ys[i]));
        }
    }
    let f = correct_format(f ^ 0x5412)?;
    let data = f >> 10;
    Some(((data >> 3) as u8, (data & 7) as u8))
}

pub(crate) fn decode(code: &Code) -> Result<usize, DecodeError> {
    let n = code.size;
    if n < 21 || !(n - 17).is_multiple_of(4) || (n - 17) / 4 > 40 {
        return Err(DecodeError::InvalidGridSize);
    }
    let version = (n - 17) / 4;
    let (ecc, mask) = read_format(code, false)
        .or_else(|| read_format(code, true))
        .ok_or(DecodeError::Format)?;
    // Codeword stream.
    let mut raw = Vec::with_capacity(n * n / 8);
    let mut acc = 0u8;
    let mut bits = 0;
    let (mut x, mut y, mut dir) = (n as i32 - 1, n as i32 - 1, -1i32);
    while x > 0 {
        if x == 6 {
            x -= 1;
        }
        for xx in [x, x - 1] {
            let (i, j) = (y as usize, xx as usize);
            if !reserved(version, i, j) {
                let v = code.bit(j, i) ^ mask_bit(mask, i, j);
                acc = (acc << 1) | u8::from(v);
                bits += 1;
                if bits == 8 {
                    raw.push(acc);
                    acc = 0;
                    bits = 0;
                }
            }
        }
        y += dir;
        if y < 0 || y >= n as i32 {
            dir = -dir;
            x -= 2;
            y += dir;
        }
    }
    let (ec, c1, d1, c2, d2) = VERSIONS[version - 1].ecc[ecc as usize];
    let (ec, c1, d1, c2, d2) = (
        ec as usize,
        c1 as usize,
        d1 as usize,
        c2 as usize,
        d2 as usize,
    );
    let blocks = c1 + c2;
    let total_data = c1 * d1 + c2 * d2;
    if raw.len() < total_data + ec * blocks {
        return Err(DecodeError::Ecc);
    }
    let mut data = Vec::with_capacity(total_data);
    let mut block = Vec::with_capacity(d2.max(d1) + ec);
    for b in 0..blocks {
        let dw = if b < c1 { d1 } else { d2 };
        block.clear();
        for j in 0..dw {
            let idx = if j < d1 {
                j * blocks + b
            } else {
                d1 * blocks + (b - c1)
            };
            block.push(raw[idx]);
        }
        for j in 0..ec {
            block.push(raw[total_data + j * blocks + b]);
        }
        if !rs::correct(&mut block, ec) {
            return Err(DecodeError::Ecc);
        }
        data.extend_from_slice(&block[..dw]);
    }
    validate_payload(&data, version)
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn remaining(&self) -> usize {
        self.d.len() * 8 - self.pos
    }
    fn take(&mut self, n: usize) -> Option<u32> {
        if n > self.remaining() {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.d[self.pos / 8];
            v = (v << 1) | u32::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }
}

/// Walk the segments; returns the payload length in bytes/characters.
fn validate_payload(data: &[u8], version: usize) -> Result<usize, DecodeError> {
    let mut b = Bits { d: data, pos: 0 };
    let class = if version < 10 {
        0
    } else if version < 27 {
        1
    } else {
        2
    };
    let mut chars = 0usize;
    while b.remaining() >= 4 {
        let mode = b.take(4).ok_or(DecodeError::Payload)?;
        let (count_bits, unit_bits): (usize, fn(usize) -> usize) = match mode {
            0 => break,
            1 => ([10, 12, 14][class], |n| n / 3 * 10 + [0, 4, 7][n % 3]),
            2 => ([9, 11, 13][class], |n| n / 2 * 11 + (n % 2) * 6),
            4 => ([8, 16, 16][class], |n| n * 8),
            8 => ([8, 10, 12][class], |n| n * 13),
            7 => {
                // ECI designator: 1, 2 or 3 bytes.
                let first = b.take(8).ok_or(DecodeError::Payload)?;
                let extra = if first & 0x80 == 0 {
                    0
                } else if first & 0xC0 == 0x80 {
                    8
                } else if first & 0xE0 == 0xC0 {
                    16
                } else {
                    return Err(DecodeError::Payload);
                };
                b.take(extra).ok_or(DecodeError::Payload)?;
                continue;
            }
            3 => {
                b.take(16).ok_or(DecodeError::Payload)?; // structured append
                continue;
            }
            5 => continue,
            9 => {
                b.take(8).ok_or(DecodeError::Payload)?;
                continue;
            }
            _ => break, // unknown mode: stop, as quirc does
        };
        let n = b.take(count_bits).ok_or(DecodeError::Payload)? as usize;
        let need = unit_bits(n);
        if need > b.remaining() {
            return Err(DecodeError::Payload);
        }
        if mode == 1 || mode == 2 {
            // Validate numeric / alphanumeric digit groups.
            let mut left = n;
            while left > 0 {
                let (take, bits, max) = if mode == 1 {
                    match left {
                        1 => (1, 4, 10),
                        2 => (2, 7, 100),
                        _ => (3, 10, 1000),
                    }
                } else if left == 1 {
                    (1, 6, 45)
                } else {
                    (2, 11, 45 * 45)
                };
                let v = b.take(bits).ok_or(DecodeError::Payload)?;
                if v >= max {
                    return Err(DecodeError::Payload);
                }
                left -= take;
            }
        } else {
            b.pos += need;
        }
        chars += n;
    }
    Ok(chars)
}
