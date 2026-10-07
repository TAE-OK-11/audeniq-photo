//! `ImageFind::FindImages`: halftone/photo regions as a mask.

use super::bitmap::Bitmap;
use super::morph::{Op, halftone_mask};

const MIN_IMAGE_FIND_SIZE: usize = 100;

pub fn find_images(pix: &Bitmap) -> Bitmap {
    let (width, height) = (pix.width, pix.height);
    if width / 2 < MIN_IMAGE_FIND_SIZE || height / 2 < MIN_IMAGE_FIND_SIZE {
        return Bitmap::new(width, height);
    }
    let pixr = pix.reduce_rank_cascade([1, 0, 0, 0]);
    let Some(ht2) = halftone_mask(&pixr) else {
        return Bitmap::new(width, height);
    };
    if ht2.is_zero() {
        return Bitmap::new(width, height);
    }
    let mut ht = ht2.expand_replicate(2);
    let t = Bitmap::seedfill(&ht, pix, 8);
    ht.combine(&t, Op::Or);
    let fine = ht.reduce_rank_cascade([1, 1, 3, 3]).dilate_brick(5, 5);
    let reduced = ht.reduce_rank_cascade([1, 1, 1, 1]);
    let reduced2 = reduced.reduce_rank_cascade([3, 3, 3, 0]).dilate_brick(5, 5);
    let mut coarse = reduced2.expand_replicate(8);
    coarse.combine(&fine, Op::And);
    let coarse = coarse.dilate_brick(3, 3);
    let mask = coarse.expand_replicate(16);
    ht.combine(&mask, Op::And);
    let mut result = Bitmap::new(width, height);
    result.combine(&ht, Op::Or);
    result
}
