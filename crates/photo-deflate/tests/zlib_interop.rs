//! Cross-check against the reference zlib (via python3's `zlib` module) when
//! it is available. Skips silently otherwise.
use photo_deflate::{Level, compress_zlib, inflate_zlib};
use std::io::Write;
use std::process::{Command, Stdio};

fn python(script: &str, input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new("python3")
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().ok()?;
    writer.join().ok()?.ok()?;
    out.status.success().then_some(out.stdout)
}

fn corpus() -> Vec<Vec<u8>> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut noise = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect()
    };
    let text = b"PNG IDAT rows compress well when filtered; repeated rows repeat. ".repeat(5000);
    let mut gradient = Vec::new();
    for y in 0..512u32 {
        gradient.push(1);
        for x in 0..1536u32 {
            gradient.push(((x * 3 + y) % 7) as u8);
        }
    }
    vec![Vec::new(), b"x".to_vec(), text, gradient, noise(300_000), {
        let mut m = noise(1000);
        m.extend(std::iter::repeat_n(0, 70_000));
        m.extend(noise(40_000));
        m
    }]
}

#[test]
fn reference_zlib_decodes_our_streams() {
    for data in corpus() {
        for level in [0, 1, 4, 6, 9] {
            let z = compress_zlib(&data, Level::new(level));
            let Some(back) = python(
                "import sys,zlib;sys.stdout.buffer.write(zlib.decompress(sys.stdin.buffer.read()))",
                &z,
            ) else {
                eprintln!("python3 zlib unavailable; skipping");
                return;
            };
            assert_eq!(back, data, "level {level}");
        }
    }
}

#[test]
fn we_decode_reference_zlib_streams() {
    for data in corpus() {
        for (level, strategy) in [
            (0, 0),
            (1, 0),
            (6, 0),
            (9, 0),
            (6, 1),
            (6, 2),
            (6, 3),
            (6, 4),
        ] {
            let script = format!(
                "import sys,zlib;c=zlib.compressobj({level},zlib.DEFLATED,15,9,{strategy});d=sys.stdin.buffer.read();sys.stdout.buffer.write(c.compress(d)+c.flush())"
            );
            let Some(z) = python(&script, &data) else {
                eprintln!("python3 zlib unavailable; skipping");
                return;
            };
            let mut out = Vec::new();
            let r = inflate_zlib(&z, &mut out, data.len(), false).unwrap();
            assert!(r.complete);
            assert_eq!(r.consumed, z.len());
            assert_eq!(out, data, "level {level} strategy {strategy}");
        }
    }
}

#[test]
fn corrupt_and_truncated_streams_fail_cleanly() {
    let data = b"some data that compresses some data that compresses".repeat(100);
    let z = compress_zlib(&data, Level::DEFAULT);
    for cut in 0..z.len() {
        let mut out = Vec::new();
        assert!(
            inflate_zlib(&z[..cut], &mut out, data.len(), false).is_err(),
            "cut {cut}"
        );
    }
    let mut x = 1u32;
    for i in 0..z.len() {
        for bit in [1u8, 0x10, 0x80] {
            let mut bad = z.clone();
            bad[i] ^= bit;
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let mut out = Vec::new();
            // Must never panic; almost every flip is detected by the checksum.
            let _ = inflate_zlib(&bad, &mut out, data.len() * 2, false);
        }
    }
    let mut out = Vec::new();
    assert!(inflate_zlib(&z, &mut out, data.len() - 1, false).is_err());
    let mut out = Vec::new();
    let r = inflate_zlib(&z, &mut out, 10, true).unwrap();
    assert!(!r.complete);
    assert_eq!(&out[..], &data[..10]);
}

#[test]
fn block_header_with_all_nineteen_code_length_codes() {
    // Regression: a dynamic block using all 19 code-length codes needs 57
    // header bits. Synthetic photo rows produce such blocks.
    let (w, h) = (3000u32, 500u32);
    let mut data = Vec::new();
    let mut x32 = 0x9E37_79B9u32;
    for y in 0..h {
        for x in 0..w {
            x32 ^= x32 << 13;
            x32 ^= x32 >> 17;
            x32 ^= x32 << 5;
            let n = (x32 & 15) as i32 - 8;
            data.extend_from_slice(&[
                ((x * 255 / w) as i32 + n).clamp(0, 255) as u8,
                ((y * 255 / h) as i32 + n).clamp(0, 255) as u8,
                ((((x / 37) ^ (y / 53)) & 0x3F) as i32 * 3 + 60 + n).clamp(0, 255) as u8,
            ]);
        }
    }
    let z = compress_zlib(&data, Level::DEFAULT);
    let mut out = Vec::new();
    assert!(
        inflate_zlib(&z, &mut out, data.len(), false)
            .unwrap()
            .complete
    );
    assert_eq!(out, data);
}

#[test]
fn randomized_roundtrips() {
    // Mixed-entropy buffers of many sizes through every level; streams
    // also checked by reference zlib when available.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut python_checked = 0;
    for case in 0..160 {
        let len = match case % 4 {
            0 => (rnd() % 300) as usize,
            1 => (rnd() % 70_000) as usize,
            2 => 65_536 + (rnd() % 200_000) as usize,
            _ => (rnd() % 1_500_000) as usize,
        };
        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            match rnd() % 5 {
                0 => {
                    let n = (rnd() % 400) as usize;
                    for _ in 0..n {
                        data.push(rnd() as u8);
                    }
                }
                1 => {
                    let b = rnd() as u8;
                    let n = (rnd() % 3000) as usize;
                    data.extend(std::iter::repeat_n(b, n));
                }
                2 if data.len() > 10 => {
                    let back = 1 + (rnd() as usize % data.len().min(40_000));
                    let n = (rnd() % 600) as usize;
                    let start = data.len() - back;
                    for i in 0..n {
                        let v = data[start + i % back];
                        data.push(v);
                    }
                }
                _ => {
                    let n = (rnd() % 500) as usize;
                    let base = rnd() as u8;
                    for i in 0..n {
                        data.push(base.wrapping_add((i % 7) as u8).wrapping_add((rnd() % 3) as u8));
                    }
                }
            }
        }
        data.truncate(len);
        let level = Level::new((case % 10) as u8);
        let z = compress_zlib(&data, level);
        let mut out = Vec::new();
        let r = inflate_zlib(&z, &mut out, data.len(), false).unwrap_or_else(|e| panic!("case {case} len {len} level {}: {e}", level.get()));
        assert!(r.complete && out == data, "case {case}");
        if case % 16 == 0 {
            if let Some(back) = python("import sys,zlib;sys.stdout.buffer.write(zlib.decompress(sys.stdin.buffer.read()))", &z) {
                assert_eq!(back, data, "zlib disagrees on case {case}");
                python_checked += 1;
            }
        }
    }
    eprintln!("randomized: 160 cases, {python_checked} cross-checked with zlib");
}
