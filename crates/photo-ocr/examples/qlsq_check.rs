//! Prints QLSQ fits for comparison with x87 long double (dev check).
use photo_ocr::page::fit::Qlsq;
fn main() {
    let mut s: u32 = 99;
    for t in 0..3000u32 {
        let mut q = Qlsq::default();
        let n = t % 40 + 1;
        let base = ((s >> 8) % 3000) as i32;
        for i in 0..n as i32 {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            let x = base + i * (((s >> 16) % 30) as i32 + 1);
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            let y = ((s >> 16) % 2000) as i32 + if t % 7 == 0 { i * i / 3 } else { 0 };
            q.add(f64::from(x), f64::from(y));
        }
        q.fit((t % 3) as i32);
        println!("{:.16e} {:.16e} {:.16e}", q.a, q.b, q.c);
    }
}
