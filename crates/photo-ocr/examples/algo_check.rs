//! Prints libstdc++-compatible sort/nth_element orders (dev check).
use photo_ocr::page::stdalgo::{nth_element, sort};
fn main() {
    let mut s: u32 = 777;
    for trial in 0..200usize {
        let n = trial * 7 % 300 + 1;
        let mut v: Vec<(i32, i32)> = Vec::new();
        for i in 0..n {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            v.push((((s >> 16) % (trial as u32 % 10 + 1)) as i32, i as i32));
        }
        let mut w = v.clone();
        sort(&mut v, |a, b| a.0 < b.0);
        println!(
            "{}",
            v.iter().map(|p| format!("{} ", p.1)).collect::<String>()
        );
        nth_element(&mut w, n / 2, |a, b| a.0 < b.0);
        println!(
            "{}",
            w.iter().map(|p| format!("{} ", p.1)).collect::<String>()
        );
    }
}
