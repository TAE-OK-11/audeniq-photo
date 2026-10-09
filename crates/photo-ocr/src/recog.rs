//! Page recognition (`Tesseract::recog_all_words` for LSTM models) and the
//! TSV renderer (`TessBaseAPI::GetTSVText`).
//!
//! Each textord word (or run of similar words, `ROW_RES`'s
//! `merge_similar_words`) is cut from the original image with its row's
//! ascender/descender band and recognized by the most recently successful
//! language first, retrying the others while the result is not acceptable
//! (`classify_word_and_language`). The recognized words then take over the
//! source blobs (`PAGE_RES_IT::ReplaceCurrentWord`), which give the boxes.

use crate::dict::valid_word_permuter;
use crate::page::geom::TBox;
use crate::page::wordseg::{PageRow, TextBlock, W_REP_CHAR, Werd};
use crate::unicharset::{UNICHAR_SPACE, Unicharset};
use crate::{Model, OcrRandom, Pix};

const IMAGE_PADDING: i32 = 4;
const MAX_WORD_SIZE_RATIO: f64 = 1.25;
const MAX_LINE_SIZE_RATIO: f64 = 1.25;
const MAX_WORD_GAP_RATIO: f64 = 2.0;
const MAX_RATING_RATIO: f64 = 1.5;
const MAX_CERTAINTY_MARGIN: f64 = 5.5;
const STOPPER_NONDICT_CERTAINTY_BASE: f64 = -2.50;
const STOPPER_SMALLWORD_SIZE: i32 = 2;
const STOPPER_CERTAINTY_PER_CHAR: f64 = -0.50;
const TOP_CHOICE_PERM: i32 = 2;

/// A recognized word ready for output.
#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    /// `100 + 5 * certainty`, clipped to 0..100.
    pub confidence: f32,
    /// Box in page coordinates (origin bottom left).
    pub bbox: TBox,
}

/// A text line of recognized words with its `ROW` box.
#[derive(Clone, Debug)]
pub struct OcrLine {
    pub words: Vec<OcrWord>,
    pub bbox: TBox,
    /// `ROW::bounding_box()` as textord left it (paragraph boxes use it).
    pub row_box: TBox,
}

/// A block of recognized lines.
#[derive(Clone, Debug)]
pub struct OcrBlock {
    pub lines: Vec<OcrLine>,
}

/// `WERD_CHOICE` as far as the result needs it.
#[derive(Clone, Debug)]
struct Choice {
    ids: Vec<i32>,
    text: String,
    rating: f32,
    certainty: f32,
    permuter: i32,
}

impl Choice {
    fn all_spaces(&self) -> bool {
        self.ids.iter().all(|&u| u == UNICHAR_SPACE)
    }
}

/// A recognition candidate (`WERD_RES` from `ExtractBestPathAsWords`, or
/// the input set up as a fake word).
#[derive(Clone, Debug)]
struct Cand {
    choice: Choice,
    /// Per-character blob boxes.
    blobs: Vec<TBox>,
    bbox: TBox,
    failed: bool,
    accepted: bool,
    combination: bool,
}

/// A `ROW` word: its box as `restricted_bounding_box(false, false)` sees it.
#[derive(Clone, Debug)]
struct RowItem {
    key: u64,
    true_box: TBox,
    rej: Vec<TBox>,
}

impl RowItem {
    fn restricted_box(&self) -> TBox {
        let mut b = self.true_box;
        let (top, bottom) = (self.true_box.top, self.true_box.bottom);
        for d in &self.rej {
            if d.bottom <= top && d.top >= bottom {
                b.union_with(d);
            }
        }
        b
    }
}

/// `WERD_RES`.
#[derive(Clone, Debug)]
struct WordRes {
    /// Blob boxes of the word (`cblob_list`) and its rejected blobs.
    blobs: Vec<TBox>,
    rej: Vec<TBox>,
    rep_char: bool,
    combination: bool,
    part_of_combo: bool,
    choice: Option<Choice>,
    failed: bool,
    accepted: bool,
    /// The `ROW` word this result stands for, when it is in the row list.
    row_key: Option<u64>,
}

impl WordRes {
    fn bounding_box(&self) -> TBox {
        let mut b = TBox::default();
        for x in self.blobs.iter().chain(&self.rej) {
            b.union_with(x);
        }
        b
    }

    fn restricted_box(&self) -> TBox {
        let mut t = TBox::default();
        for x in &self.blobs {
            t.union_with(x);
        }
        RowItem {
            key: 0,
            true_box: t,
            rej: self.rej.clone(),
        }
        .restricted_box()
    }
}

fn werd_boxes(w: &Werd) -> (Vec<TBox>, Vec<TBox>) {
    (
        w.cblobs.iter().map(|b| b.bounding_box()).collect(),
        w.rej_cblobs.iter().map(|b| b.bounding_box()).collect(),
    )
}

struct RowRes<'a> {
    row: &'a PageRow,
    words: Vec<WordRes>,
    items: Vec<RowItem>,
}

/// `ROW_RES(merge_similar_words = true, row)`.
fn row_res<'a>(row: &'a PageRow, next_key: &mut u64) -> RowRes<'a> {
    let line_height = f64::from(row.xheight + row.ascrise - row.descdrop);
    let n = row.words.len();
    let mut words: Vec<WordRes> = Vec::new();
    let mut items = Vec::new();
    let mut combo: Option<usize> = None;
    let mut add_next_word = false;
    let mut union_box = TBox::default();
    for (i, w) in row.words.iter().enumerate() {
        let (blobs, rej) = werd_boxes(w);
        let key = *next_key;
        *next_key += 1;
        let mut tb = TBox::default();
        for b in &blobs {
            tb.union_with(b);
        }
        items.push(RowItem {
            key,
            true_box: tb,
            rej: rej.clone(),
        });
        let mut res = WordRes {
            blobs,
            rej,
            rep_char: w.flag(W_REP_CHAR),
            combination: false,
            part_of_combo: false,
            choice: None,
            failed: false,
            accepted: false,
            row_key: Some(key),
        };
        if add_next_word {
            res.part_of_combo = true;
            let c = combo.expect("combo in progress");
            copy_on(&mut words[c], &res);
        } else {
            union_box = w.bounding_box();
            add_next_word =
                !res.rep_char && f64::from(union_box.height()) <= line_height * MAX_WORD_SIZE_RATIO;
        }
        let next = &row.words[(i + 1) % n];
        if add_next_word && !next.flag(W_REP_CHAR) {
            let next_box = next.bounding_box();
            let prev_right = union_box.right;
            union_box.union_with(&next_box);
            if f64::from(next_box.height()) > line_height * MAX_WORD_SIZE_RATIO
                || f64::from(union_box.height()) > line_height * MAX_LINE_SIZE_RATIO
                || f64::from(next_box.left)
                    > f64::from(prev_right) + line_height * MAX_WORD_GAP_RATIO
            {
                add_next_word = false;
            }
        }
        if add_next_word {
            if combo.is_none() {
                let mut c = res.clone();
                c.part_of_combo = false;
                c.combination = true;
                c.row_key = None;
                words.push(c);
                combo = Some(words.len() - 1);
            }
            res.part_of_combo = true;
        } else {
            combo = None;
        }
        words.push(res);
    }
    RowRes { row, words, items }
}

/// `WERD::copy_on`: the other word's blobs go before or after by position.
fn copy_on(dst: &mut WordRes, other: &WordRes) {
    let reversed = other.bounding_box().left < dst.bounding_box().left;
    if reversed {
        let mut b = other.blobs.clone();
        b.extend_from_slice(&dst.blobs);
        dst.blobs = b;
        if !other.rej.is_empty() {
            let mut r = other.rej.clone();
            r.extend_from_slice(&dst.rej);
            dst.rej = r;
        }
    } else {
        dst.blobs.extend_from_slice(&other.blobs);
        dst.rej.extend_from_slice(&other.rej);
    }
}

/// Recognizes the words of the text blocks on the original image.
/// `None` when `stop` asks to give up (checked before each word).
pub fn recognize_page(
    original: &Pix,
    blocks: &[TextBlock],
    models: &[&Model],
    stop: &dyn Fn() -> bool,
) -> Option<Vec<OcrBlock>> {
    let mut next_key = 0u64;
    let mut rows: Vec<Vec<RowRes>> = blocks
        .iter()
        .map(|b| b.rows.iter().map(|r| row_res(r, &mut next_key)).collect())
        .collect();
    let width = original.width() as i32;
    let height = original.height() as i32;
    let mut rand: Vec<OcrRandom> = models.iter().map(|_| OcrRandom::default()).collect();
    let mut most_recent = 0usize;
    for block_rows in &mut rows {
        for rr in block_rows.iter_mut() {
            let mut i = 0;
            while i < rr.words.len() {
                if rr.words[i].part_of_combo {
                    i += 1;
                    continue;
                }
                if stop() {
                    return None;
                }
                let best = classify_word(
                    &rr.words[i],
                    rr.row,
                    original,
                    (width, height),
                    models,
                    &mut rand,
                    &mut most_recent,
                );
                i = apply_result(rr, i, best);
            }
            // recog_all_words: drop empty and all-space results.
            let mut j = 0;
            while j < rr.words.len() {
                let w = &rr.words[j];
                let empty = !w.part_of_combo
                    && w.choice
                        .as_ref()
                        .is_none_or(|c| c.ids.is_empty() || c.all_spaces());
                if empty {
                    let w = rr.words.remove(j);
                    if !w.combination
                        && let Some(k) = w.row_key
                    {
                        rr.items.retain(|it| it.key != k);
                    }
                } else {
                    j += 1;
                }
            }
        }
    }
    let out = rows
        .iter()
        .map(|block_rows| OcrBlock {
            lines: block_rows
                .iter()
                .map(|rr| {
                    let mut bbox = TBox::default();
                    for it in &rr.items {
                        bbox.union_with(&it.restricted_box());
                    }
                    OcrLine {
                        bbox,
                        row_box: rr.row.bound_box,
                        words: rr
                            .words
                            .iter()
                            .filter(|w| !w.part_of_combo)
                            .map(|w| {
                                let c = w.choice.as_ref().expect("recognized word");
                                OcrWord {
                                    text: c.text.clone(),
                                    confidence: (100.0f32 + 5.0 * c.certainty).clamp(0.0, 100.0),
                                    bbox: w.restricted_box(),
                                }
                            })
                            .collect(),
                    }
                })
                .collect(),
        })
        .collect();
    Some(out)
}

/// `classify_word_and_language`: the chosen candidates for one word.
fn classify_word(
    word: &WordRes,
    row: &PageRow,
    original: &Pix,
    dims: (i32, i32),
    models: &[&Model],
    rand: &mut [OcrRandom],
    most_recent: &mut usize,
) -> Vec<Cand> {
    let mut best: Vec<Cand> = Vec::new();
    let mut retry = |lang: usize, best: &mut Vec<Cand>| -> i32 {
        let mut new_words =
            lstm_recognize_word(word, row, original, dims, models[lang], &mut rand[lang]);
        if new_words.is_empty() {
            new_words.push(fake_word(word));
        }
        select_best_words(new_words, best)
    };
    let mru = *most_recent;
    retry(mru, &mut best);
    let mut best_lang = mru;
    if !words_acceptable(&best) {
        if mru != 0 && retry(0, &mut best) > 0 {
            best_lang = 0;
        }
        let mut i = 1;
        while !words_acceptable(&best) && i < models.len() {
            if mru != i && retry(i, &mut best) > 0 {
                best_lang = i;
            }
            i += 1;
        }
    }
    *most_recent = best_lang;
    best
}

fn words_acceptable(words: &[Cand]) -> bool {
    words.iter().all(|w| !w.failed && w.accepted)
}

/// `WERD_RES::SetupFake`: every blob a space, rating 10, certainty -1.
fn fake_word(word: &WordRes) -> Cand {
    let n = word.blobs.len();
    let mut rating = 0.0f32;
    for _ in 0..n {
        rating += 10.0;
    }
    Cand {
        choice: Choice {
            ids: vec![UNICHAR_SPACE; n],
            text: " ".repeat(n),
            rating,
            certainty: if n > 0 { -1.0 } else { f32::MAX },
            permuter: TOP_CHOICE_PERM,
        },
        blobs: word.blobs.clone(),
        bbox: word.bounding_box(),
        failed: true,
        accepted: false,
        combination: word.combination,
    }
}

/// `LSTMRecognizeWord` + `SearchWords`.
fn lstm_recognize_word(
    word: &WordRes,
    row: &PageRow,
    original: &Pix,
    (width, height): (i32, i32),
    model: &Model,
    rand: &mut OcrRandom,
) -> Vec<Cand> {
    let mut word_box = word.bounding_box();
    let baseline = row.base_line(((word_box.left + word_box.right) / 2) as f32);
    if baseline + row.descdrop < word_box.bottom as f32 {
        word_box.bottom = (baseline + row.descdrop) as i32;
    }
    if baseline + row.xheight + row.ascrise > word_box.top as f32 {
        word_box.top = (baseline + row.xheight + row.ascrise) as i32;
    }
    // GetRectImage.
    word_box.pad(IMAGE_PADDING, IMAGE_PADDING);
    let image_box = TBox::new(0, 0, width, height);
    if !word_box.overlap(&image_box) {
        return Vec::new();
    }
    let line_box = word_box.intersection(&image_box);
    if line_box.null_box() {
        return Vec::new();
    }
    let crop = original.crop(
        line_box.left as usize,
        (height - line_box.top) as usize,
        line_box.width() as usize,
        line_box.height() as usize,
    );
    let (words, scale_factor) = model.recognize_raw(&crop, rand);
    words
        .into_iter()
        .map(|w| {
            let mut blobs = Vec::new();
            for i in 0..w.ids.len() {
                if i + 1 < w.bounds.len() {
                    let l = i32::from((w.bounds[i] as f32 * scale_factor).floor() as i16)
                        + line_box.left;
                    let r = i32::from((w.bounds[i + 1] as f32 * scale_factor).ceil() as i16)
                        + line_box.left;
                    blobs.push(TBox::new(l, line_box.bottom, r, line_box.top));
                }
            }
            let mut bbox = TBox::default();
            for b in &blobs {
                bbox.union_with(b);
            }
            let choice = Choice {
                ids: w.ids,
                text: w.text,
                rating: w.rating,
                certainty: w.certainty,
                permuter: w.permuter,
            };
            let accepted = acceptable_result(model, &choice);
            Cand {
                choice,
                blobs,
                bbox,
                failed: false,
                accepted,
                combination: true,
            }
        })
        .collect()
}

/// `Dict::AcceptableResult`.
fn acceptable_result(model: &Model, c: &Choice) -> bool {
    if c.ids.is_empty() {
        return false;
    }
    let mut threshold = STOPPER_NONDICT_CERTAINTY_BASE as f32;
    let valid = model
        .dict
        .as_ref()
        .is_some_and(|d| d.valid_word(&model.set, &c.ids) != 0);
    if valid && case_ok(&model.set, &c.ids) {
        let word_size = (shortest_alpha_run(&model.set, &c.ids) - STOPPER_SMALLWORD_SIZE).max(0);
        threshold =
            (f64::from(threshold) + f64::from(word_size) * STOPPER_CERTAINTY_PER_CHAR) as f32;
    }
    c.certainty > threshold
}

/// `Dict::case_ok`.
fn case_ok(set: &Unicharset, ids: &[i32]) -> bool {
    const TABLE: [[i32; 4]; 6] = [
        [0, 1, 5, 4],
        [0, 3, 2, 4],
        [0, -1, 2, -1],
        [0, 3, -1, 4],
        [0, -1, -1, 4],
        [5, -1, 2, -1],
    ];
    let mut state = 0i32;
    for &id in ids {
        let col = if set.is_upper(id) {
            1
        } else if set.is_lower(id) {
            2
        } else if set.is_digit(id) {
            3
        } else {
            0
        };
        state = TABLE[state as usize][col];
        if state == -1 {
            return false;
        }
    }
    state != 5
}

/// `Dict::LengthOfShortestAlphaRun`.
fn shortest_alpha_run(set: &Unicharset, ids: &[i32]) -> i32 {
    let mut shortest = i32::MAX;
    let mut curr = 0;
    for &id in ids {
        if set.is_alpha(id) {
            curr += 1;
        } else if curr > 0 {
            shortest = shortest.min(curr);
            curr = 0;
        }
    }
    if curr > 0 && curr < shortest {
        shortest = curr;
    } else if shortest == i32::MAX {
        shortest = 0;
    }
    shortest
}

fn word_gap(words: &[Cand], index: usize) -> (i32, i32) {
    let mut right = -i32::MAX;
    let mut next_left = i32::MAX;
    if index < words.len() {
        right = words[index].bbox.right;
        if index + 1 < words.len() {
            next_left = words[index + 1].bbox.left;
        }
    }
    (right, next_left)
}

/// `EvaluateWordSpan`: (rating, certainty, bad, valid permuter).
fn evaluate_span(words: &[Cand], first: usize, end: usize) -> (f32, f32, bool, bool) {
    let mut rating = 0.0f32;
    let mut certainty = 0.0f32;
    let mut bad = false;
    let mut valid = true;
    if end <= first {
        bad = true;
        valid = false;
    }
    for w in words.iter().take(end).skip(first) {
        rating += w.choice.rating;
        certainty = certainty.min(w.choice.certainty);
        if !valid_word_permuter(w.choice.permuter) {
            valid = false;
        }
    }
    (rating, certainty, bad, valid)
}

/// `SelectBestWords`: merges span by span; returns new minus kept old.
fn select_best_words(new_words: Vec<Cand>, best: &mut Vec<Cand>) -> i32 {
    let mut out = Vec::new();
    let (mut b, mut n) = (0usize, 0usize);
    let (mut num_best, mut num_new) = (0, 0);
    while b < best.len() || n < new_words.len() {
        let (start_b, start_n) = (b, n);
        while b < best.len() || n < new_words.len() {
            let (b_right, next_b_left) = word_gap(best, b);
            let (n_right, next_n_left) = word_gap(&new_words, n);
            if b_right.max(n_right) < next_b_left.min(next_n_left) {
                break;
            }
            if (b_right < n_right && b < best.len()) || n == new_words.len() {
                b += 1;
            } else {
                n += 1;
            }
        }
        let end_b = if b < best.len() { b + 1 } else { b };
        let end_n = if n < new_words.len() { n + 1 } else { n };
        let (b_rating, b_cert, b_bad, b_valid) = evaluate_span(best, start_b, end_b);
        let (n_rating, n_cert, n_bad, n_valid) = evaluate_span(&new_words, start_n, end_n);
        if !n_bad
            && (b_bad
                || (n_cert > b_cert && n_rating < b_rating)
                || (!b_valid
                    && n_valid
                    && f64::from(n_rating) < f64::from(b_rating) * MAX_RATING_RATIO
                    && f64::from(n_cert) > f64::from(b_cert) - MAX_CERTAINTY_MARGIN))
        {
            for w in &new_words[start_n..end_n] {
                out.push(w.clone());
                num_new += 1;
            }
        } else if !b_bad {
            for w in &best[start_b..end_b] {
                out.push(w.clone());
                num_best += 1;
            }
        }
        b = end_b;
        n = end_n;
    }
    *best = out;
    num_new - num_best
}

/// Stores the result for word `i`; returns the index of the next word.
fn apply_result(rr: &mut RowRes, i: usize, best: Vec<Cand>) -> usize {
    if best.is_empty() {
        return i + 1;
    }
    if best.len() == 1 && !best[0].combination {
        // ConsumeWordResults.
        let c = &best[0];
        let w = &mut rr.words[i];
        w.choice = Some(c.choice.clone());
        w.failed = c.failed;
        w.accepted = c.accepted;
        return i + 1;
    }
    replace_current_word(rr, i, best)
}

/// `PAGE_RES_IT::ReplaceCurrentWord`.
fn replace_current_word(rr: &mut RowRes, i: usize, words: Vec<Cand>) -> usize {
    let input = rr.words[i].clone();
    let mut src: Vec<TBox> = input.blobs.clone();
    src.sort_by_key(|b| (b.left + b.right) / 2);
    let mut rej: Vec<TBox> = input.rej.clone();
    rej.sort_by_key(|b| (b.left + b.right) / 2);
    let (mut si, mut ri) = (0usize, 0usize);
    // Following part-of-combo results (the combination's source words).
    let parts: Vec<TBox> = rr.words[i + 1..]
        .iter()
        .take_while(|w| w.part_of_combo)
        .map(|w| w.bounding_box())
        .collect();
    let mut clip_box = TBox::default();
    let mut new_res = Vec::new();
    for w in 0..words.len() {
        clip_box = compute_word_bounds(&words, w, clip_box, &parts);
        let next_blobs = words.get(w + 1).map(|n| &n.blobs);
        let ends = compute_blob_ends(&words[w], &clip_box, next_blobs);
        let mut moved = Vec::new();
        for (k, &end_x) in ends.iter().enumerate() {
            let mut blob_box = TBox::default();
            while si < src.len() && (src[si].left + src[si].right) / 2 < end_x {
                let b = move_and_clip(src[si], &clip_box);
                blob_box.union_with(&b);
                moved.push(b);
                si += 1;
            }
            while ri < rej.len() && (rej[ri].left + rej[ri].right) / 2 < end_x {
                let b = move_and_clip(rej[ri], &clip_box);
                blob_box.union_with(&b);
                moved.push(b);
                ri += 1;
            }
            if blob_box.null_box()
                && let Some(&f) = words[w].blobs.get(k)
            {
                moved.push(move_and_clip(f, &clip_box));
            }
        }
        new_res.push(WordRes {
            blobs: moved,
            rej: Vec::new(),
            rep_char: false,
            combination: input.combination,
            part_of_combo: false,
            choice: Some(words[w].choice.clone()),
            failed: words[w].failed,
            accepted: words[w].accepted,
            row_key: None,
        });
    }
    if !input.combination {
        // The new words replace the input in the row's word list.
        let pos = rr
            .items
            .iter()
            .position(|it| Some(it.key) == input.row_key)
            .expect("input word in its row");
        rr.items.remove(pos);
        let mut keys = Vec::new();
        for (k, nw) in new_res.iter_mut().enumerate() {
            let key = u64::MAX - (input.row_key.unwrap_or(0) << 16) - k as u64;
            keys.push(key);
            nw.row_key = Some(key);
            let mut tb = TBox::default();
            for b in &nw.blobs {
                tb.union_with(b);
            }
            rr.items.insert(
                pos + k,
                RowItem {
                    key,
                    true_box: tb,
                    rej: Vec::new(),
                },
            );
        }
    }
    let n = new_res.len();
    rr.words.splice(i..=i, new_res);
    i + n
}

/// `ComputeWordBounds`.
fn compute_word_bounds(words: &[Cand], w: usize, prev_box: TBox, parts: &[TBox]) -> TBox {
    let mut clipped = TBox::default();
    let current = words[w].bbox;
    let next_box = words.get(w + 1).map_or_else(TBox::default, |n| n.bbox);
    for &w_box in parts {
        let height_limit = w_box.height().min(w_box.width() / 2);
        let width_limit = w_box.width() / 4;
        let min_sig = height_limit.max(width_limit);
        let overlap = w_box.intersection(&current).width();
        let prev_overlap = w_box.intersection(&prev_box).width();
        let next_overlap = w_box.intersection(&next_box).width();
        if overlap > min_sig {
            if prev_overlap > min_sig {
                clipped.left = current.left;
            } else if next_overlap > min_sig {
                clipped.right = current.right;
            } else {
                clipped.union_with(&w_box);
            }
        }
    }
    if clipped.height() <= 0 {
        clipped.top = current.top;
        clipped.bottom = current.bottom;
    }
    if clipped.width() <= 0 {
        clipped = current;
    }
    clipped
}

/// `ClipToRange`: the lower bound wins when the range is empty.
fn clip(x: i32, lower: i32, upper: i32) -> i32 {
    if x < lower {
        lower
    } else if x > upper {
        upper
    } else {
        x
    }
}

/// `ComputeBlobEnds` (one blob per character).
fn compute_blob_ends(word: &Cand, clip_box: &TBox, next_blobs: Option<&Vec<TBox>>) -> Vec<i32> {
    let n = word.choice.ids.len();
    let mut ends = Vec::with_capacity(n);
    for k in 0..n {
        let blob_box = word.blobs[k % word.blobs.len()];
        let at_first = k + 1 == word.blobs.len();
        let mut blob_end = i32::MAX;
        if !at_first || next_blobs.is_some() {
            let next = if at_first {
                next_blobs.expect("checked")[0]
            } else {
                word.blobs[k + 1]
            };
            blob_end = (blob_box.right + next.left) / 2;
        }
        ends.push(clip(blob_end, clip_box.left, clip_box.right));
    }
    if let Some(last) = ends.last_mut() {
        *last = clip_box.right;
    }
    ends
}

/// `MoveAndClipBlob` (boxes only).
fn move_and_clip(b: TBox, c: &TBox) -> TBox {
    if c.contains(&b) {
        return b;
    }
    let left = clip(b.left, c.left, c.right - 1);
    let right = clip(b.right, c.left + 1, c.right);
    let top = clip(b.top, c.bottom + 1, c.top);
    let bottom = clip(b.bottom, c.bottom, c.top - 1);
    TBox::new(left, bottom, right, top)
}

/// `TessBaseAPI::GetTSVText` for page 1 of an image `width` x `height`.
pub fn tsv(blocks: &[OcrBlock], width: i32, height: i32) -> String {
    let mut s = format!("1\t1\t0\t0\t0\t0\t0\t0\t{width}\t{height}\t-1\t\n");
    let to_tsv = |b: &TBox| -> (i32, i32, i32, i32) {
        let left = clip(b.left, 0, width);
        let top = clip(height - b.top, 0, height);
        let right = clip(b.right, left, width);
        let bottom = clip(height - b.bottom, top, height);
        (left, top, right - left, bottom - top)
    };
    let mut block_num = 0;
    for block in blocks {
        if block.lines.iter().all(|l| l.words.is_empty()) {
            continue;
        }
        block_num += 1;
        let mut bbox = TBox::default();
        for l in &block.lines {
            bbox.union_with(&l.bbox);
        }
        let (l, t, w, h) = to_tsv(&bbox);
        s += &format!("2\t1\t{block_num}\t0\t0\t0\t{l}\t{t}\t{w}\t{h}\t-1\t\n");
        // One paragraph per block: the first line's box with every row box.
        let first = block
            .lines
            .iter()
            .find(|l| !l.words.is_empty())
            .expect("non-empty");
        let mut para = first.bbox;
        for l in &block.lines {
            para.union_with(&l.row_box);
        }
        let (l, t, w, h) = to_tsv(&para);
        s += &format!("3\t1\t{block_num}\t1\t0\t0\t{l}\t{t}\t{w}\t{h}\t-1\t\n");
        let mut line_num = 0;
        for line in block.lines.iter().filter(|l| !l.words.is_empty()) {
            line_num += 1;
            let (l, t, w, h) = to_tsv(&line.bbox);
            s += &format!("4\t1\t{block_num}\t1\t{line_num}\t0\t{l}\t{t}\t{w}\t{h}\t-1\t\n");
            for (wi, word) in line.words.iter().enumerate() {
                let (l, t, w, h) = to_tsv(&word.bbox);
                s += &format!(
                    "5\t1\t{block_num}\t1\t{line_num}\t{}\t{l}\t{t}\t{w}\t{h}\t{:.6}\t{}\n",
                    wi + 1,
                    word.confidence,
                    word.text
                );
            }
        }
    }
    s
}
