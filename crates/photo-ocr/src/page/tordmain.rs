//! The end of `Textord::TextordPage` (`tordmain.cpp`): noise cleanup of the
//! word rows, baseline tweaking and the transfer of diacritics to words.

use super::blobbox::{BlobId, Blobs};
use super::elist::EList;
use super::fit::QSpline;
use super::geom::TBox;
use super::grid::{BBGrid, BoxOf, GridSearch};
use super::layout::LayoutBlock;
use super::outline::{CBlob, Outline};
use super::textord::{ToBlk, baseline_detect};
use super::wordseg::{PageRow, TextBlock, W_DONT_CHOP, W_FUZZY_NON, W_REP_CHAR, Werd, make_words};

const TEXTORD_NOISE_SIZELIMIT: f64 = 0.5;
const TEXTORD_NOISE_SXFRACT: f64 = 0.4;
const TEXTORD_NOISE_SYFRACT: f64 = 0.2;
const TEXTORD_NOISE_SIZEFRACTION: i32 = 10;
const TEXTORD_NOISE_TRANSLIMIT: i32 = 16;
const TEXTORD_NOISE_SNCOUNT: i32 = 1;
const TEXTORD_NOISE_ROWRATIO: f64 = 6.0;
const TEXTORD_NOISE_NORMRATIO: f64 = 2.0;
const TEXTORD_NOISE_HFRACT: f64 = 1.0 / 64.0;

/// `Textord::TextordPage` for the sparse-text page segmentation mode.
pub fn textord_page(
    layout_blocks: Vec<LayoutBlock>,
    diacritics: &EList<BlobId>,
    blobs: &mut Blobs,
) -> Vec<TextBlock> {
    let mut blocks: Vec<ToBlk> = layout_blocks.into_iter().map(ToBlk::from_layout).collect();
    if blocks.is_empty() {
        return Vec::new();
    }
    baseline_detect(&mut blocks, blobs);
    let mut out = make_words(&mut blocks, blobs);
    cleanup_blocks(&mut out);
    transfer_diacritics(diacritics, &mut out, blobs);
    out
}

fn cleanup_blocks(blocks: &mut Vec<TextBlock>) {
    blocks.retain_mut(|block| {
        if !block.ptype.is_text() {
            return true;
        }
        block.rows.retain_mut(|row| {
            clean_small_noise_from_words(row);
            if row.words.is_empty() || clean_noise_from_row(row) {
                return false;
            }
            clean_noise_from_words(row);
            tweak_row_baseline(row);
            true
        });
        !block.rows.is_empty()
    });
}

fn clean_small_noise_from_words(row: &mut PageRow) {
    let mut i = 0;
    while i < row.words.len() {
        let word = &mut row.words[i];
        let min_size =
            (TEXTORD_NOISE_HFRACT * f64::from(word.bounding_box().height()) + 0.5) as i32;
        for blob in &mut word.cblobs {
            Outline::remove_small(&mut blob.outlines, min_size);
        }
        word.cblobs.retain(|b| !b.outlines.is_empty());
        if word.cblobs.is_empty() {
            if i + 1 < row.words.len() && row.words[i + 1].flag(W_FUZZY_NON) {
                row.words[i + 1].set_flag(W_FUZZY_NON, false);
            }
            row.words.remove(i);
        } else {
            i += 1;
        }
    }
}

/// Dot/normal/super-normal counts of a word's blobs (`clean_noise_from_*`).
fn noise_counts(
    word: &Werd,
    xh: f64,
    first_word: bool,
    dot: &mut i32,
    norm: &mut i32,
    sup: &mut i32,
) {
    for (bi, blob) in word.cblobs.iter().enumerate() {
        if !word.flag(W_DONT_CHOP) {
            for o in &blob.outlines {
                let b = o.bbox;
                let size = b.width().max(b.height());
                if f64::from(size) < TEXTORD_NOISE_SIZELIMIT * xh {
                    *dot += 1;
                }
                let (h, w) = (f64::from(b.height()), f64::from(b.width()));
                if !o.children.is_empty()
                    && h < (1.0 + TEXTORD_NOISE_SYFRACT) * xh
                    && h > (1.0 - TEXTORD_NOISE_SYFRACT) * xh
                    && w < (1.0 + TEXTORD_NOISE_SXFRACT) * xh
                    && w > (1.0 - TEXTORD_NOISE_SXFRACT) * xh
                {
                    *sup += 1;
                }
            }
        } else {
            *sup += 1;
        }
        let b = blob.bounding_box();
        let size = b.width().max(b.height());
        if f64::from(size) >= TEXTORD_NOISE_SIZELIMIT * xh && f64::from(size) < xh * 2.0 {
            let trans_threshold = size / TEXTORD_NOISE_SIZEFRACTION;
            if blob.count_transitions(trans_threshold) < TEXTORD_NOISE_TRANSLIMIT {
                *norm += 1;
            }
        } else if f64::from(b.height()) > xh * 2.0 && (!first_word || bi != 0) {
            *dot += 2;
        }
    }
}

fn clean_noise_from_row(row: &PageRow) -> bool {
    let xh = f64::from(row.xheight);
    let (mut dot, mut norm, mut sup) = (0, 0, 0);
    for (wi, word) in row.words.iter().enumerate() {
        noise_counts(word, xh, wi == 0, &mut dot, &mut norm, &mut sup);
    }
    sup < TEXTORD_NOISE_SNCOUNT
        && f64::from(dot) > f64::from(norm) * TEXTORD_NOISE_ROWRATIO
        && dot > 2
}

fn clean_noise_from_words(row: &mut PageRow) {
    let xh = f64::from(row.xheight);
    if row.words.is_empty() {
        return;
    }
    let mut word_dud = vec![0i8; row.words.len()];
    let (mut dud_words, mut ok_words) = (0, 0);
    for (wi, word) in row.words.iter().enumerate() {
        let (mut dot, mut norm, mut sup) = (0, 0, 0);
        noise_counts(word, xh, wi == 0, &mut dot, &mut norm, &mut sup);
        // In this function the super-normal count is the normal count.
        let norm = norm + sup;
        word_dud[wi] = if dot > 2 && !word.flag(W_REP_CHAR) {
            if f64::from(dot) > f64::from(norm) * TEXTORD_NOISE_NORMRATIO * 2.0 {
                2
            } else if f64::from(dot) > f64::from(norm) * TEXTORD_NOISE_NORMRATIO {
                1
            } else {
                0
            }
        } else {
            0
        };
        if word_dud[wi] == 2 {
            dud_words += 1;
        } else {
            ok_words += 1;
        }
    }
    for (wi, word) in row.words.iter_mut().enumerate() {
        if word_dud[wi] == 2 || (word_dud[wi] == 1 && dud_words > ok_words) {
            clean_noise(word, (TEXTORD_NOISE_SIZELIMIT * xh) as f32);
        }
    }
}

/// `WERD::CleanNoise`.
fn clean_noise(word: &mut Werd, size_threshold: f32) {
    let mut kept = Vec::new();
    for mut blob in std::mem::take(&mut word.cblobs) {
        let mut outlines = Vec::new();
        for o in blob.outlines {
            let b = o.bbox;
            let size = b.width().max(b.height());
            if (size as f32) < size_threshold {
                word.rej_cblobs.push(CBlob { outlines: vec![o] });
            } else {
                outlines.push(o);
            }
        }
        blob.outlines = outlines;
        if !blob.outlines.is_empty() {
            kept.push(blob);
        }
    }
    word.cblobs = kept;
}

/// `tweak_row_baseline` with `textord_blshift_maxshift` 0: rebuilds the
/// spline from the segments under the blobs.
fn tweak_row_baseline(row: &mut PageRow) {
    let blob_count: usize = row.words.iter().map(|w| w.cblobs.len()).sum();
    if blob_count == 0 {
        return;
    }
    let bl = &row.baseline;
    let segments = bl.segments() as usize;
    let mut xstarts = vec![0i32; blob_count + segments + 1];
    let mut coeffs = vec![0f64; (blob_count + segments) * 3];
    let mut src = 0usize;
    let mut dest = 0usize;
    xstarts[0] = bl.xcoords[0];
    let set = |coeffs: &mut Vec<f64>, d: usize, s: usize| {
        let q = bl.quads[s];
        coeffs[d * 3] = q.a;
        coeffs[d * 3 + 1] = f64::from(q.b);
        coeffs[d * 3 + 2] = f64::from(q.c);
    };
    for word in &row.words {
        for blob in &word.cblobs {
            let b = blob.bounding_box();
            let x_centre = (f64::from(b.left + b.right) / 2.0) as f32;
            if xstarts[dest] as f32 <= x_centre {
                while bl.xcoords[src + 1] as f32 <= x_centre && src + 1 < segments {
                    if bl.xcoords[src + 1] > xstarts[dest] {
                        set(&mut coeffs, dest, src);
                        dest += 1;
                        xstarts[dest] = bl.xcoords[src + 1];
                    }
                    src += 1;
                }
                set(&mut coeffs, dest, src);
                dest += 1;
                xstarts[dest] = bl.xcoords[src + 1];
            }
        }
    }
    while src < segments && bl.xcoords[src + 1] <= xstarts[dest] {
        src += 1;
    }
    while src < segments {
        set(&mut coeffs, dest, src);
        dest += 1;
        src += 1;
        xstarts[dest] = bl.xcoords[src];
    }
    row.baseline = QSpline::from_coeffs(&xstarts[..=dest], &coeffs[..dest * 3]);
}

struct WordBoxes(Vec<TBox>);

impl BoxOf<u32> for WordBoxes {
    fn box_of(&self, t: u32) -> TBox {
        self.0[t as usize]
    }
}

/// `TransferDiacriticsToBlockGroups`: every block shares the unrotated
/// group.
fn transfer_diacritics(diacritics: &EList<BlobId>, blocks: &mut [TextBlock], blobs: &Blobs) {
    let mut bbox = TBox::default();
    let mut min_xheight = 0f32;
    let mut first = true;
    for b in blocks.iter() {
        if !b.ptype.is_text() {
            continue;
        }
        if first {
            bbox = b.bbox;
            min_xheight = b.xheight as f32;
            first = false;
        } else {
            bbox.union_with(&b.bbox);
            if (b.xheight as f32) < min_xheight {
                min_xheight = b.xheight as f32;
            }
        }
    }
    if first || bbox.null_box() {
        return;
    }
    let mut refs: Vec<(usize, usize, usize)> = Vec::new();
    let mut boxes = Vec::new();
    for (bi, b) in blocks.iter().enumerate() {
        if !b.ptype.is_text() {
            continue;
        }
        for (ri, r) in b.rows.iter().enumerate() {
            for (wi, w) in r.words.iter().enumerate() {
                refs.push((bi, ri, wi));
                // WordWithBox pads the box by its height all round.
                let mut b = w.bounding_box();
                let h = b.height();
                b.pad(h, h);
                boxes.push(b);
            }
        }
    }
    let boxes = WordBoxes(boxes);
    let mut grid: BBGrid<u32> = BBGrid::new(min_xheight as i32, bbox.botleft(), bbox.topright());
    for i in 0..refs.len() as u32 {
        grid.insert_bbox(&boxes, true, true, i);
    }
    for id in diacritics.to_vec() {
        let blob = blobs.get(id);
        let blob_box = blob.bbox;
        let mut ws: GridSearch<u32> = GridSearch::new();
        ws.start_rect_search(&grid, &blob_box);
        let mut best_above: Option<u32> = None;
        let mut best_below: Option<u32> = None;
        let mut best_above_distance = 0;
        let mut best_below_distance = 0;
        while let Some(wid) = ws.next_rect_search(&grid, &boxes) {
            let (bi, ri, wi) = refs[wid as usize];
            let word = &blocks[bi].rows[ri].words[wi];
            if word.flag(W_REP_CHAR) {
                continue;
            }
            let word_box = word.true_bounding_box();
            let mut x_distance = blob_box.x_gap(&word_box);
            let mut y_distance = blob_box.y_gap(&word_box);
            if x_distance > 0 {
                if word_box.major_y_overlap(&blob_box) && blob_box.left > word_box.right {
                    x_distance /= 2;
                }
                y_distance += x_distance;
            }
            let wy = (word_box.bottom + word_box.top) / 2;
            let by = (blob_box.bottom + blob_box.top) / 2;
            if wy > by && (best_above.is_none() || y_distance < best_above_distance) {
                best_above = Some(wid);
                best_above_distance = y_distance;
            }
            if wy <= by && (best_below.is_none() || y_distance < best_below_distance) {
                best_below = Some(wid);
                best_below_distance = y_distance;
            }
        }
        let above_good = best_above.is_some()
            && (best_below.is_none()
                || best_above_distance < best_below_distance + blob_box.height());
        let below_good = best_below.is_some()
            && best_below != best_above
            && (best_above.is_none()
                || best_below_distance < best_above_distance + blob_box.height());
        let cb = blob.cblob.as_ref().expect("diacritic blob");
        let copy = || CBlob {
            outlines: cb
                .outlines
                .iter()
                .map(|o| o.rotated_tree((1.0, -0.0)))
                .collect(),
        };
        if below_good {
            let (bi, ri, wi) = refs[best_below.expect("below") as usize];
            blocks[bi].rows[ri].words[wi].rej_cblobs.push(copy());
        }
        if above_good {
            let (bi, ri, wi) = refs[best_above.expect("above") as usize];
            blocks[bi].rows[ri].words[wi].rej_cblobs.push(copy());
        }
    }
}
