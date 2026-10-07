//! `ImageThresholder` Otsu binarization (`otsuthr.cpp`, `thresholder.cpp`).

use super::bitmap::Bitmap;
use crate::pix::Pix;

/// `OtsuStats`: (best threshold, total count, omega_0 at the best split).
fn otsu_stats(hist: &[i32; 256]) -> (i32, i32, i32) {
    let mut h = 0i32;
    let mut mu_total = 0.0f64;
    for (i, &c) in hist.iter().enumerate() {
        h += c;
        mu_total += i as f64 * f64::from(c);
    }
    let mut best_t = -1;
    let mut best_omega_0 = 0;
    let mut best_sig = 0.0f64;
    let mut omega_0 = 0;
    let mut mu_t = 0.0f64;
    for t in 0..255 {
        omega_0 += hist[t];
        mu_t += t as f64 * f64::from(hist[t]);
        if omega_0 == 0 {
            continue;
        }
        let omega_1 = h - omega_0;
        if omega_1 == 0 {
            break;
        }
        let mu_0 = mu_t / f64::from(omega_0);
        let mu_1 = (mu_total - mu_t) / f64::from(omega_1);
        let mut sig = mu_1 - mu_0;
        sig *= sig * f64::from(omega_0) * f64::from(omega_1);
        if best_t < 0 || sig > best_sig {
            best_sig = sig;
            best_t = t as i32;
            best_omega_0 = omega_0;
        }
    }
    (best_t, h, best_omega_0)
}

/// Per-channel planes as Leptonica stores them: grey is one channel;
/// 32 bpp RGB is four (R, G, B and the always-zero spare byte).
fn channels(pix: &Pix) -> Vec<&[u8]> {
    match pix {
        Pix::Gray(g) => vec![&g.data[..]],
        Pix::Rgb(p) => vec![&p[0].data[..], &p[1].data[..], &p[2].data[..]],
    }
}

/// `OtsuThreshold`: per-channel thresholds and "hi" polarity (−1 = ignore).
pub(crate) fn otsu_threshold(pix: &Pix) -> (Vec<i32>, Vec<i32>) {
    let chans = channels(pix);
    // The spare byte of 32 bpp pixels is a constant channel: Otsu skips it.
    let n = if chans.len() == 3 { 4 } else { 1 };
    let mut thresholds = vec![-1; n];
    let mut hi_values = vec![-1; n];
    let mut best_hi_value = 1;
    let mut best_hi_index = 0;
    let mut any_good = false;
    let mut best_hi_dist = 0.0f64;
    for ch in 0..n {
        let mut hist = [0i32; 256];
        match chans.get(ch) {
            Some(data) => data.iter().for_each(|&v| hist[v as usize] += 1),
            None => hist[0] = (pix.width() * pix.height()) as i32,
        }
        let (best_t, h, omega_0) = otsu_stats(&hist);
        if omega_0 == 0 || omega_0 == h {
            continue;
        }
        let hi_value = i32::from(f64::from(omega_0) < f64::from(h) * 0.5);
        thresholds[ch] = best_t;
        if f64::from(omega_0) > f64::from(h) * 0.75 {
            any_good = true;
            hi_values[ch] = 0;
        } else if f64::from(omega_0) < f64::from(h) * 0.25 {
            any_good = true;
            hi_values[ch] = 1;
        } else {
            let hi_dist = if hi_value != 0 {
                f64::from(h - omega_0)
            } else {
                f64::from(omega_0)
            };
            if hi_dist > best_hi_dist {
                best_hi_dist = hi_dist;
                best_hi_value = hi_value;
                best_hi_index = ch;
            }
        }
    }
    if !any_good {
        hi_values[best_hi_index] = best_hi_value;
    }
    (thresholds, hi_values)
}

/// `ThresholdToPix` → binary page image (1 = black).
pub fn threshold(pix: &Pix) -> Bitmap {
    let (thresholds, hi) = otsu_threshold(pix);
    let chans = channels(pix);
    let (w, h) = (pix.width(), pix.height());
    let mut out = Bitmap::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let black = (0..thresholds.len()).any(|ch| {
                let v = chans.get(ch).map_or(0, |d| i32::from(d[i]));
                hi[ch] >= 0 && (v > thresholds[ch]) == (hi[ch] == 0)
            });
            if black {
                out.set(x, y);
            }
        }
    }
    out
}

/// `GetPixRectThresholds`: one global threshold of the grey image.
pub fn grey_threshold(grey: &crate::pix::Gray) -> i32 {
    let (t, _) = otsu_threshold(&Pix::Gray(grey.clone()));
    if t[0] > 0 { t[0] } else { 128 }
}
