use std::io::Write;
fn main() {
    let w = 3000u32;
    let h: u32 = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(3000);
    let mut data = Vec::new();
    let mut x32 = 0x9E37_79B9u32;
    for y in 0..h {
        for x in 0..w {
            x32 ^= x32 << 13; x32 ^= x32 >> 17; x32 ^= x32 << 5;
            let n = (x32 & 15) as i32 - 8;
            data.extend_from_slice(&[((x * 255 / w) as i32 + n).clamp(0, 255) as u8, ((y * 255 / h) as i32 + n).clamp(0, 255) as u8, ((((x / 37) ^ (y / 53)) & 0x3F) as i32 * 3 + 60 + n).clamp(0, 255) as u8]);
        }
    }
    let z = photo_deflate::compress_zlib(&data, photo_deflate::Level::DEFAULT);
    std::fs::File::create("/tmp/claude-0/z.bin").unwrap().write_all(&z).unwrap();
    std::fs::File::create("/tmp/claude-0/raw.bin").unwrap().write_all(&data).unwrap();
    let mut out = Vec::new();
    println!("{:?}", photo_deflate::inflate_zlib(&z, &mut out, data.len(), false).map(|r| r.complete));
}
