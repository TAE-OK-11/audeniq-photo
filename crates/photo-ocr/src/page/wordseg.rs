//! Word segmentation (`wordseg.cpp`, `topitch.cpp`, `pithsync.cpp`,
//! `tospace.cpp`, `gap_map.cpp`, `fpchop.cpp::fixed_pitch_words`): pitch
//! decisions per row, space thresholds, and the rows of words.

use super::blobbox::{BlobId, Blobs};
use super::colpartition::PolyBlockType;
use super::detlinefit::int_cast_rounded_f32;
use super::elist::{EList, Iter};
use super::fit::QSpline;
use super::fpchop::{OutlineArena, split_to_blob};
use super::geom::{ICoord, TBox};
use super::outline::{CBlob, find_cblob_hlimits};
use super::stats::Stats;
use super::textord::{
    PitchType, TEXTORD_FP_CHOP_ERROR, TEXTORD_MIN_XHEIGHT, ToBlk, ToRow, box_next, mark_repeated,
};

pub const W_BOL: u32 = 3;
pub const W_EOL: u32 = 4;
pub const W_DONT_CHOP: u32 = 8;
pub const W_REP_CHAR: u32 = 9;
pub const W_FUZZY_SP: u32 = 10;
pub const W_FUZZY_NON: u32 = 11;
pub const W_INVERSE: u32 = 12;

// topitch.cpp / wordseg.cpp
const BLOCK_STATS_CLUSTERS: usize = 10;
const TEXTORD_WORDS_MAXSPACE: f64 = 4.0;
const TEXTORD_WORDS_DEFAULT_MINSPACE: f64 = 0.6;
const TEXTORD_WORDS_DEFAULT_NONSPACE: f64 = 0.2;
const TEXTORD_WORDSTATS_SMOOTH_FACTOR: f64 = 0.05;
const TEXTORD_SPACESIZE_RATIOPROP: f64 = 2.0;
const TEXTORD_WORDS_MIN_MINSPACE: f64 = 0.3;
const TEXTORD_WORDS_PITCHSD_THRESHOLD: f64 = 0.040;
const TEXTORD_WORDS_DEF_FIXED: f64 = 0.016;
const TEXTORD_WORDS_DEF_PROP: f64 = 0.090;
const TEXTORD_WORDS_VETO_POWER: i32 = 5;
const TEXTORD_PITCH_ROWSIMILARITY: f64 = 0.08;
const TEXTORD_FPIQR_RATIO: f64 = 1.5;
const TEXTORD_MAX_PITCH_IQR: f64 = 0.20;
const TEXTORD_WORDS_DEFAULT_MAXSPACE: f64 = 3.5;
const TEXTORD_DOTMATRIX_GAP: i32 = 3;
const TEXTORD_PITCH_RANGE: i32 = 2;
const TEXTORD_PROJECTION_SCALE: f64 = 0.200;
const TEXTORD_BALANCE_FACTOR: f64 = 1.0;
const WORDS_DEFAULT_PROP_NONSPACE: f64 = 0.25;
const WORDS_DEFAULT_FIXED_SPACE: f64 = 0.75;
const WORDS_DEFAULT_FIXED_LIMIT: f64 = 0.6;
const WORDS_INITIAL_LOWER: f64 = 0.5;
const WORDS_INITIAL_UPPER: f64 = 0.15;
const PITSYNC_JOINED_EDGE: f64 = 0.75;

// tospace.cpp
const MAXSPACING: i32 = 128;
const TOSP_ENOUGH_SPACE_SAMPLES_FOR_MEDIAN: i32 = 3;
const TOSP_FEW_SAMPLES: i32 = 40;
const TOSP_SHORT_ROW: i32 = 20;
const TOSP_REDO_KERN_LIMIT: i32 = 10;
const TOSP_ENOUGH_SMALL_GAPS: f64 = 0.65;
const TOSP_FUZZY_SPACE_FACTOR: f64 = 0.6;
const TOSP_FUZZY_SPACE_FACTOR1: f64 = 0.5;
const TOSP_FUZZY_SPACE_FACTOR2: f64 = 0.72;
const TOSP_GAP_FACTOR: f64 = 0.83;
const TOSP_IGNORE_VERY_BIG_GAPS: f64 = 3.5;
const TOSP_INIT_GUESS_KN_MULT: f64 = 2.2;
const TOSP_INIT_GUESS_XHT_MULT: f64 = 0.28;
const TOSP_KERN_GAP_FACTOR1: f64 = 2.0;
const TOSP_KERN_GAP_FACTOR2: f64 = 1.3;
const TOSP_KERN_GAP_FACTOR3: f64 = 2.5;
const TOSP_LARGE_KERNING: f64 = 0.19;
const TOSP_MAX_SANE_KN_THRESH: f64 = 5.0;
const TOSP_MIN_SANE_KN_SP: f64 = 1.5;
const TOSP_NARROW_ASPECT_RATIO: f64 = 0.48;
const TOSP_NARROW_FRACTION: f64 = 0.3;
const TOSP_NEAR_LH_EDGE: f64 = 0.0;
const TOSP_OLD_SP_KN_TH_FACTOR: f64 = 2.0;
const TOSP_PASS_WIDE_FUZZ_SP_TO_CONTEXT: f64 = 0.75;
const TOSP_REP_SPACE: f64 = 1.6;
const TOSP_SILLY_KN_SP_GAP: f64 = 0.2;
const TOSP_TABLE_FUZZY_KN_SP_RATIO: f64 = 3.0;
const TOSP_TABLE_KN_SP_RATIO: f64 = 2.25;
const TOSP_TABLE_XHT_SP_RATIO: f64 = 0.33;
const TOSP_WIDE_FRACTION: f64 = 0.52;
const TOSP_FUZZY_SP_FRACTION: f64 = 0.5;
const TOSP_FUZZY_KN_FRACTION: f64 = 0.5;
const GAPMAP_BIG_GAPS: f64 = 1.75;

/// `WERD`.
#[derive(Clone, Debug, Default)]
pub struct Werd {
    pub cblobs: Vec<CBlob>,
    pub rej_cblobs: Vec<CBlob>,
    pub blanks: u8,
    pub flags: u32,
}

impl Werd {
    /// `WERD(C_BLOB_LIST*, blanks, nullptr)`: rejects blobs whose
    /// inversion disagrees with the majority.
    pub fn new(cblobs: Vec<CBlob>, blanks: u8) -> Werd {
        let mut w = Werd {
            cblobs: Vec::new(),
            rej_cblobs: Vec::new(),
            blanks,
            flags: 0,
        };
        if cblobs.is_empty() {
            return w;
        }
        let mut inverted_vote = 0;
        let mut non_inverted_vote = 0;
        let mut kept = Vec::new();
        for b in cblobs {
            let inv = b.outlines[0].inverse;
            if b.outlines.iter().any(|o| o.inverse != inv) {
                w.rej_cblobs.push(b);
            } else {
                if inv {
                    inverted_vote += 1;
                } else {
                    non_inverted_vote += 1;
                }
                kept.push(b);
            }
        }
        let inverse = inverted_vote > non_inverted_vote;
        w.set_flag(W_INVERSE, inverse);
        for b in kept {
            if b.outlines[0].inverse != inverse {
                w.rej_cblobs.push(b);
            } else {
                w.cblobs.push(b);
            }
        }
        w
    }

    pub fn flag(&self, f: u32) -> bool {
        self.flags & (1 << f) != 0
    }

    pub fn set_flag(&mut self, f: u32, v: bool) {
        if v {
            self.flags |= 1 << f;
        } else {
            self.flags &= !(1 << f);
        }
    }

    pub fn true_bounding_box(&self) -> TBox {
        let mut b = TBox::default();
        for c in &self.cblobs {
            b.union_with(&c.bounding_box());
        }
        b
    }

    pub fn bounding_box(&self) -> TBox {
        let mut b = self.true_bounding_box();
        for c in &self.rej_cblobs {
            b.union_with(&c.bounding_box());
        }
        b
    }
}

/// `ROW`.
#[derive(Clone, Debug)]
pub struct PageRow {
    pub words: Vec<Werd>,
    pub baseline: QSpline,
    pub xheight: f32,
    pub ascrise: f32,
    pub descdrop: f32,
    pub bodysize: f32,
    pub kerning: i16,
    pub spacing: i16,
    pub bound_box: TBox,
}

impl PageRow {
    fn new(row: &ToRow, kern: i16, space: i16) -> PageRow {
        PageRow {
            words: Vec::new(),
            baseline: row.baseline.clone(),
            xheight: row.xheight,
            ascrise: row.ascrise,
            descdrop: row.descdrop,
            bodysize: row.body_size,
            kerning: kern,
            spacing: space,
            bound_box: TBox::default(),
        }
    }

    /// `ROW::recalc_bounding_box`.
    pub fn recalc_bounding_box(&mut self) {
        let lefts: Vec<i32> = self.words.iter().map(|w| w.bounding_box().left).collect();
        if lefts.windows(2).any(|w| w[1] < w[0]) {
            self.words.sort_by_key(|w| w.bounding_box().left);
        }
        let n = self.words.len();
        for (i, w) in self.words.iter_mut().enumerate() {
            w.set_flag(W_BOL, i == 0);
            w.set_flag(W_EOL, i + 1 == n);
            self.bound_box.union_with(&w.bounding_box());
        }
    }

    pub fn base_line(&self, x: f32) -> f32 {
        self.baseline.y(f64::from(x)) as f32
    }
}

/// `BLOCK` with its rows of words.
#[derive(Clone, Debug)]
pub struct TextBlock {
    pub bbox: TBox,
    pub ptype: PolyBlockType,
    pub rows: Vec<PageRow>,
    pub xheight: i32,
    pub kerning: i16,
    pub spacing: i16,
    pub pitch: i16,
    pub proportional: bool,
}

/// `make_words`.
pub fn make_words(blocks: &mut [ToBlk], blobs: &mut Blobs) -> Vec<TextBlock> {
    compute_fixed_pitch(blocks, blobs);
    to_spacing(blocks, blobs);
    blocks
        .iter_mut()
        .map(|b| make_real_words(b, blobs))
        .collect()
}

// ---------------------------------------------------------------- topitch

fn compute_fixed_pitch(blocks: &mut [ToBlk], blobs: &mut Blobs) {
    for block in blocks.iter_mut() {
        compute_block_pitch(block, blobs);
    }
    // try_doc_fixed: textord_blockndoc_fixed is off.
    for block in blocks.iter_mut() {
        try_rows_fixed(block, blobs);
    }
    for bi in 0..blocks.len() {
        if !blocks[bi].is_text() {
            continue;
        }
        for ri in 0..blocks[bi].rows.len() {
            fix_row_pitch(blocks, bi, ri, blobs);
        }
    }
}

fn fix_row_pitch(blocks: &mut [ToBlk], bi: usize, ri: usize, blobs: &Blobs) {
    let (mut block_votes, mut like_votes, mut other_votes) = (0, 0, 0);
    let bad = &blocks[bi].rows[ri];
    let maxwidth = (f64::from(bad.xheight) * TEXTORD_WORDS_MAXSPACE).ceil() as i32;
    let mut block_stats = Stats::default();
    let mut like_stats = Stats::default();
    let veto = TEXTORD_WORDS_VETO_POWER;
    if bad.pitch_decision != PitchType::DefFixed && bad.pitch_decision != PitchType::DefProp {
        block_stats.set_range(0, maxwidth - 1);
        like_stats.set_range(0, maxwidth - 1);
        let sim = TEXTORD_PITCH_ROWSIMILARITY;
        let bad_asc = f64::from(bad.xheight + bad.ascrise);
        let bad_xh = f64::from(bad.xheight);
        for (obi, block) in blocks.iter().enumerate() {
            if !block.is_text() {
                continue;
            }
            for row in &block.rows {
                let row_asc = f64::from(row.xheight + row.ascrise);
                let row_xh = f64::from(row.xheight);
                let like = (bad.all_caps
                    && row_asc < bad_asc * (1.0 + sim)
                    && row_asc > bad_asc * (1.0 - sim))
                    || (!bad.all_caps
                        && row_xh < bad_xh * (1.0 + sim)
                        && row_xh > bad_xh * (1.0 - sim));
                let pd = row.pitch_decision;
                if like {
                    if obi == bi {
                        match pd {
                            PitchType::DefFixed => {
                                block_votes += veto;
                                block_stats.add(row.fixed_pitch as i32, veto);
                            }
                            PitchType::MaybeFixed | PitchType::CorrFixed => {
                                block_votes += 1;
                                block_stats.add(row.fixed_pitch as i32, 1);
                            }
                            PitchType::DefProp => block_votes -= veto,
                            PitchType::MaybeProp | PitchType::CorrProp => block_votes -= 1,
                            PitchType::Dunno => {}
                        }
                    } else {
                        match pd {
                            PitchType::DefFixed => {
                                like_votes += veto;
                                like_stats.add(row.fixed_pitch as i32, veto);
                            }
                            PitchType::MaybeFixed | PitchType::CorrFixed => {
                                like_votes += 1;
                                like_stats.add(row.fixed_pitch as i32, 1);
                            }
                            PitchType::DefProp => like_votes -= veto,
                            PitchType::MaybeProp | PitchType::CorrProp => like_votes -= 1,
                            PitchType::Dunno => {}
                        }
                    }
                } else {
                    match pd {
                        PitchType::DefFixed => other_votes += veto,
                        PitchType::MaybeFixed | PitchType::CorrFixed => other_votes += 1,
                        PitchType::DefProp => other_votes -= veto,
                        PitchType::MaybeProp | PitchType::CorrProp => other_votes -= 1,
                        PitchType::Dunno => {}
                    }
                }
            }
        }
        let _ = other_votes;
        let bad = &mut blocks[bi].rows[ri];
        if block_votes > veto {
            bad.fixed_pitch = block_stats.ile(0.5) as f32;
            bad.pitch_decision = PitchType::CorrFixed;
        } else if block_votes <= veto && like_votes > 0 {
            bad.fixed_pitch = like_stats.ile(0.5) as f32;
            bad.pitch_decision = PitchType::CorrFixed;
        } else {
            bad.pitch_decision = PitchType::CorrProp;
        }
    }
    let bad = &mut blocks[bi].rows[ri];
    if bad.pitch_decision == PitchType::CorrFixed {
        if bad.fixed_pitch < TEXTORD_MIN_XHEIGHT as f32 {
            if block_votes > 0 {
                bad.fixed_pitch = block_stats.ile(0.5) as f32;
            } else if block_votes == 0 && like_votes > 0 {
                bad.fixed_pitch = like_stats.ile(0.5) as f32;
            } else {
                bad.fixed_pitch = bad.xheight;
            }
        }
        if bad.fixed_pitch < TEXTORD_MIN_XHEIGHT as f32 {
            bad.fixed_pitch = TEXTORD_MIN_XHEIGHT as f32;
        }
        bad.kern_size = bad.fixed_pitch / 4.0;
        bad.min_space = (f64::from(bad.fixed_pitch) * 0.6) as i32;
        bad.max_nonspace = (f64::from(bad.fixed_pitch) * 0.4) as i32;
        bad.space_threshold = (bad.min_space + bad.max_nonspace) / 2;
        bad.space_size = bad.fixed_pitch;
        if bad.char_cells.is_empty() && !bad.blobs.is_empty() {
            let space = (bad.fixed_pitch + bad.max_nonspace as f32 * 3.0) / 4.0;
            let mut pitch = bad.fixed_pitch;
            let (pl, pr) = (bad.projection_left, bad.projection_right);
            let proj = std::mem::take(&mut bad.projection);
            let mut cells = std::mem::take(&mut bad.char_cells);
            tune_row_pitch(bad, &proj, pl, pr, space, &mut pitch, &mut cells, blobs);
            bad.projection = proj;
            bad.char_cells = cells;
            bad.fixed_pitch = pitch;
        }
    } else if bad.pitch_decision == PitchType::CorrProp || bad.pitch_decision == PitchType::DefProp
    {
        bad.fixed_pitch = 0.0;
        bad.char_cells.clear();
    }
}

fn compute_block_pitch(block: &mut ToBlk, blobs: &mut Blobs) {
    let xh = f64::from(block.xheight);
    block.min_space = (xh * TEXTORD_WORDS_DEFAULT_MINSPACE).floor() as i32;
    block.max_nonspace = (xh * TEXTORD_WORDS_DEFAULT_NONSPACE).ceil() as i32;
    block.fixed_pitch = 0.0;
    block.space_size = block.min_space as f32;
    block.kern_size = block.max_nonspace as f32;
    block.pr_nonsp = (xh * WORDS_DEFAULT_PROP_NONSPACE) as f32;
    block.pr_space = (f64::from(block.pr_nonsp) * TEXTORD_SPACESIZE_RATIOPROP) as f32;
    if !block.rows.is_empty() {
        find_repeated_chars(block, blobs);
        compute_rows_pitch(block, blobs);
    }
}

fn compute_rows_pitch(block: &mut ToBlk, blobs: &Blobs) {
    let bxh = block.xheight;
    for row in &mut block.rows {
        row.compute_vertical_projection(blobs);
        let maxwidth = (f64::from(row.xheight) * TEXTORD_WORDS_MAXSPACE).ceil() as i32;
        if row_pitch_stats(row, maxwidth, blobs)
            && find_row_pitch(row, maxwidth, TEXTORD_DOTMATRIX_GAP + 1, bxh, blobs)
        {
            if row.fixed_pitch == 0.0 {
                row.space_size = row.pr_space;
                row.kern_size = row.pr_nonsp;
            }
        } else {
            row.fixed_pitch = 0.0;
            row.pitch_decision = PitchType::Dunno;
        }
    }
}

fn try_rows_fixed(block: &mut ToBlk, blobs: &Blobs) {
    let is_text = block.is_text();
    for row in &mut block.rows {
        if row.fixed_pitch > 0.0 && fixed_pitch_row(row, is_text, blobs) && row.fixed_pitch == 0.0 {
            row.space_size = row.pr_space;
            row.kern_size = row.pr_nonsp;
        }
    }
    let (mut def_fixed, mut def_prop, mut maybe_fixed, mut maybe_prop) = (0, 0, 0, 0);
    for row in &block.rows {
        match row.pitch_decision {
            PitchType::DefProp => def_prop += 1,
            PitchType::MaybeProp => maybe_prop += 1,
            PitchType::DefFixed => def_fixed += 1,
            PitchType::MaybeFixed => maybe_fixed += 1,
            _ => {}
        }
    }
    let veto = TEXTORD_WORDS_VETO_POWER;
    block.pitch_decision = if def_fixed > def_prop * veto {
        PitchType::DefFixed
    } else if def_prop > def_fixed * veto {
        PitchType::DefProp
    } else if def_fixed > 0 || def_prop > 0 {
        PitchType::Dunno
    } else if maybe_fixed > maybe_prop * veto {
        PitchType::MaybeFixed
    } else if maybe_prop > maybe_fixed * veto {
        PitchType::MaybeProp
    } else {
        PitchType::Dunno
    };
}

fn row_pitch_stats(row: &mut ToRow, maxwidth: i32, blobs: &Blobs) -> bool {
    let mut gap_stats = Stats::new(0, maxwidth - 1);
    let mut cluster_stats: Vec<Stats> = vec![Stats::default(); BLOCK_STATS_CLUSTERS + 1];
    let smooth_factor = (f64::from(row.xheight) * TEXTORD_WORDSTATS_SMOOTH_FACTOR + 1.5) as i32;
    let list = &row.blobs;
    if !list.is_empty() {
        let mut it = Iter::new(list);
        let mut prev_x = blobs.get(it.data(list)).bbox.right;
        it.forward(list);
        while !it.at_first(list) {
            let b = blobs.get(it.data(list));
            if !b.joined {
                if b.bbox.left - prev_x < maxwidth {
                    gap_stats.add(b.bbox.left - prev_x, 1);
                }
                prev_x = b.bbox.right;
            }
            it.forward(list);
        }
    }
    if gap_stats.get_total() == 0 {
        return false;
    }
    let mut cluster_count = 0;
    let lower = (f64::from(row.xheight) * WORDS_INITIAL_LOWER) as f32;
    let upper = (f64::from(row.xheight) * WORDS_INITIAL_UPPER) as f32;
    gap_stats.smooth(smooth_factor);
    loop {
        let prev_count = cluster_count;
        cluster_count = gap_stats.cluster(
            lower,
            upper,
            TEXTORD_SPACESIZE_RATIOPROP as f32,
            BLOCK_STATS_CLUSTERS as i32,
            &mut cluster_stats,
        );
        if !(cluster_count > prev_count && cluster_count < BLOCK_STATS_CLUSTERS as i32) {
            break;
        }
    }
    if cluster_count < 1 {
        return false;
    }
    let n = cluster_count as usize;
    let mut gaps: Vec<f32> = (0..n)
        .map(|i| cluster_stats[i + 1].ile(0.5) as f32)
        .collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lower = (f64::from(row.xheight) * WORDS_DEFAULT_PROP_NONSPACE) as f32;
    let upper = (f64::from(row.xheight) * TEXTORD_WORDS_MIN_MINSPACE) as f32;
    let mut gi = 0;
    while gi < n && gaps[gi] < lower {
        gi += 1;
    }
    if gi == 0 {
        if n > 1 {
            row.pr_nonsp = gaps[0];
            row.pr_space = gaps[1];
        } else {
            row.pr_nonsp = lower;
            row.pr_space = gaps[0];
        }
    } else {
        row.pr_nonsp = gaps[gi - 1];
        while gi < n && gaps[gi] < upper {
            gi += 1;
        }
        if gi == n {
            row.pr_space = (f64::from(lower) * TEXTORD_SPACESIZE_RATIOPROP) as f32;
        } else {
            row.pr_space = gaps[gi];
        }
    }
    let upper = (f64::from(row.xheight) * WORDS_DEFAULT_FIXED_SPACE) as f32;
    let mut gi = 0;
    while gi < n && gaps[gi] < upper {
        gi += 1;
    }
    if gi == 0 {
        row.fp_nonsp = upper;
        row.fp_space = gaps[0];
    } else {
        row.fp_nonsp = gaps[gi - 1];
        row.fp_space = if gi == n { row.xheight } else { gaps[gi] };
    }
    true
}

fn find_row_pitch(
    row: &mut ToRow,
    maxwidth: i32,
    dm_gap: i32,
    block_xheight: f32,
    blobs: &Blobs,
) -> bool {
    let mut gap_stats = Stats::new(0, maxwidth - 1);
    let mut pitch_stats = Stats::new(0, maxwidth - 1);
    row.fixed_pitch = 0.0;
    let mut initial_pitch = row.fp_space;
    if f64::from(initial_pitch) > f64::from(row.xheight) * (1.0 + WORDS_DEFAULT_FIXED_LIMIT) {
        initial_pitch = row.xheight;
    }
    let mut non_space = row.fp_nonsp;
    if non_space > initial_pitch {
        non_space = initial_pitch;
    }
    let mut min_space = (initial_pitch + non_space) / 2.0;
    let (dm_gap_iqr, dm_pitch_iqr, dm_pitch);
    if !count_pitch_stats(
        row,
        &mut gap_stats,
        &mut pitch_stats,
        initial_pitch,
        min_space,
        dm_gap,
        blobs,
    ) {
        dm_gap_iqr = 0.0001f32;
        dm_pitch_iqr = maxwidth as f32 * 2.0;
        dm_pitch = initial_pitch;
    } else {
        dm_gap_iqr = (gap_stats.ile(0.75) - gap_stats.ile(0.25)) as f32;
        dm_pitch_iqr = (pitch_stats.ile(0.75) - pitch_stats.ile(0.25)) as f32;
        dm_pitch = pitch_stats.ile(0.5) as f32;
    }
    gap_stats.clear();
    pitch_stats.clear();
    let (mut gap_iqr, mut pitch_iqr);
    if !count_pitch_stats(
        row,
        &mut gap_stats,
        &mut pitch_stats,
        initial_pitch,
        min_space,
        0,
        blobs,
    ) {
        gap_iqr = 0.0001f32;
        pitch_iqr = maxwidth as f32 * 3.0;
    } else {
        gap_iqr = (gap_stats.ile(0.75) - gap_stats.ile(0.25)) as f32;
        pitch_iqr = (pitch_stats.ile(0.75) - pitch_stats.ile(0.25)) as f32;
        initial_pitch = pitch_stats.ile(0.5) as f32;
        if min_space > initial_pitch
            && count_pitch_stats(
                row,
                &mut gap_stats,
                &mut pitch_stats,
                initial_pitch,
                initial_pitch,
                0,
                blobs,
            )
        {
            min_space = initial_pitch;
            gap_iqr = (gap_stats.ile(0.75) - gap_stats.ile(0.25)) as f32;
            pitch_iqr = (pitch_stats.ile(0.75) - pitch_stats.ile(0.25)) as f32;
            initial_pitch = pitch_stats.ile(0.5) as f32;
        }
    }
    let _ = (min_space, initial_pitch);
    if pitch_iqr > maxwidth as f32 && dm_pitch_iqr > maxwidth as f32 {
        row.pitch_decision = PitchType::Dunno;
        return false;
    }
    let pitch;
    let used_dm_model;
    if pitch_iqr * dm_gap_iqr <= dm_pitch_iqr * gap_iqr {
        gap_iqr = (gap_stats.ile(0.75) - gap_stats.ile(0.25)) as f32;
        pitch_iqr = (pitch_stats.ile(0.75) - pitch_stats.ile(0.25)) as f32;
        pitch = pitch_stats.ile(0.5) as f32;
        used_dm_model = false;
    } else {
        gap_iqr = dm_gap_iqr;
        pitch_iqr = dm_pitch_iqr;
        pitch = dm_pitch;
        used_dm_model = true;
    }
    let bxh = f64::from(block_xheight);
    row.pitch_decision = if f64::from(pitch_iqr) < f64::from(gap_iqr) * TEXTORD_FPIQR_RATIO
        && f64::from(pitch_iqr) < bxh * TEXTORD_MAX_PITCH_IQR
        && f64::from(pitch) < bxh * TEXTORD_WORDS_DEFAULT_MAXSPACE
    {
        PitchType::MaybeFixed
    } else {
        PitchType::MaybeProp
    };
    row.fixed_pitch = pitch;
    row.kern_size = gap_stats.ile(0.5) as f32;
    row.min_space = ((row.fixed_pitch + non_space) as i32) / 2;
    if row.min_space as f32 > row.fixed_pitch {
        row.min_space = row.fixed_pitch as i32;
    }
    row.max_nonspace = row.min_space;
    row.space_size = row.fixed_pitch;
    row.space_threshold = (row.max_nonspace + row.min_space) / 2;
    row.used_dm_model = used_dm_model;
    true
}

fn count_pitch_stats(
    row: &ToRow,
    gap_stats: &mut Stats,
    pitch_stats: &mut Stats,
    initial_pitch: f32,
    min_space: f32,
    dm_gap: i32,
    blobs: &Blobs,
) -> bool {
    gap_stats.clear();
    pitch_stats.clear();
    let list = &row.blobs;
    if list.is_empty() {
        return false;
    }
    let mut it = Iter::new(list);
    let mut prev_valid = false;
    let mut prev_centre = 0i32;
    let mut prev_right = 0i32;
    let mut joined_box = blobs.get(it.data(list)).bbox;
    loop {
        it.forward(list);
        let b = blobs.get(it.data(list));
        if !b.joined {
            let blob_box = b.bbox;
            if (blob_box.left - joined_box.right < dm_gap && !it.at_first(list))
                || b.cblob.is_none()
            {
                joined_box.union_with(&blob_box);
            } else {
                let blob_width = joined_box.width();
                // ignore_outsize
                let width = blob_width as f32 / initial_pitch;
                let width_units: i32 = if f64::from(width) < 1.0 + WORDS_DEFAULT_FIXED_LIMIT
                    && f64::from(width) > 1.0 - WORDS_DEFAULT_FIXED_LIMIT
                {
                    0
                } else {
                    -1
                };
                let x_centre = (joined_box.left as f32
                    + (blob_width as f32 - width_units as f32 * initial_pitch) / 2.0)
                    as i32;
                if prev_valid && width_units >= 0 {
                    gap_stats.add(joined_box.left - prev_right, 1);
                    pitch_stats.add(x_centre - prev_centre, 1);
                }
                prev_centre = (x_centre as f32 + width_units as f32 * initial_pitch) as i32;
                prev_right = joined_box.right;
                prev_valid = ((blob_box.left - joined_box.right) as f32) < min_space;
                prev_valid = prev_valid && width_units >= 0;
                joined_box = blob_box;
            }
        }
        if it.at_first(list) {
            break;
        }
    }
    gap_stats.get_total() >= 3
}

fn fixed_pitch_row(row: &mut ToRow, is_text: bool, blobs: &Blobs) -> bool {
    let mut non_space = row.fp_nonsp;
    if non_space > row.fixed_pitch {
        non_space = row.fixed_pitch;
    }
    if !is_text {
        row.pitch_decision = PitchType::DefProp;
        return true;
    }
    let space = (row.fixed_pitch + non_space * 3.0) / 4.0;
    let mut pitch = row.fixed_pitch;
    let (pl, pr) = (row.projection_left, row.projection_right);
    let proj = std::mem::take(&mut row.projection);
    let mut cells = std::mem::take(&mut row.char_cells);
    let (pitch_sd, sp_sd, mid_cuts) =
        tune_row_pitch(row, &proj, pl, pr, space, &mut pitch, &mut cells, blobs);
    row.projection = proj;
    row.char_cells = cells;
    row.fixed_pitch = pitch;
    let _ = (sp_sd, mid_cuts);
    let fp = f64::from(row.fixed_pitch);
    let sd = f64::from(pitch_sd);
    // pitsync_linear_version & 3 == 2.
    if sd < TEXTORD_WORDS_PITCHSD_THRESHOLD * fp {
        row.pitch_decision = if sd < TEXTORD_WORDS_DEF_FIXED * fp && !row.all_caps {
            PitchType::DefFixed
        } else {
            PitchType::MaybeFixed
        };
    } else if sd < TEXTORD_WORDS_DEF_PROP * fp {
        row.pitch_decision = PitchType::MaybeProp;
    } else {
        row.pitch_decision = PitchType::DefProp;
    }
    true
}

/// `tune_row_pitch`: returns (best sd, space sd, mid cuts); updates the
/// pitch and the cells.
#[allow(clippy::too_many_arguments)]
fn tune_row_pitch(
    row: &ToRow,
    projection: &Stats,
    projection_left: i32,
    projection_right: i32,
    space_size: f32,
    initial_pitch: &mut f32,
    best_cells: &mut Vec<i32>,
    blobs: &Blobs,
) -> (f32, f32, i32) {
    let _ = space_size;
    let (initial_sd, mut best_sp_sd, mut best_mid_cuts) = compute_pitch_sd2(
        row,
        projection,
        projection_left,
        projection_right,
        *initial_pitch,
        best_cells,
        blobs,
    );
    let mut best_sd = initial_sd;
    let mut best_pitch = *initial_pitch;
    for sign in [1.0f32, -1.0] {
        for pitch_delta in 1..=TEXTORD_PITCH_RANGE {
            let mut test_cells = Vec::new();
            let p = *initial_pitch + sign * pitch_delta as f32;
            let (pitch_sd, sp_sd, mid_cuts) = compute_pitch_sd2(
                row,
                projection,
                projection_left,
                projection_right,
                p,
                &mut test_cells,
                blobs,
            );
            if pitch_sd < best_sd {
                best_sd = pitch_sd;
                best_mid_cuts = mid_cuts;
                best_sp_sd = sp_sd;
                best_pitch = p;
                *best_cells = test_cells;
            }
            if pitch_sd > initial_sd {
                break;
            }
        }
    }
    *initial_pitch = best_pitch;
    (best_sd, best_sp_sd, best_mid_cuts)
}

/// `compute_pitch_sd2`: returns (sd, occupation as the space sd, mid cuts).
fn compute_pitch_sd2(
    row: &ToRow,
    projection: &Stats,
    projection_left: i32,
    projection_right: i32,
    initial_pitch: f32,
    row_cells: &mut Vec<i32>,
    blobs: &Blobs,
) -> (f32, f32, i32) {
    let list = &row.blobs;
    if list.is_empty() {
        return (initial_pitch * 10.0, 0.0, 0);
    }
    let mut blob_count = 0i32;
    let mut it = Iter::new(list);
    it.mark_cycle_pt();
    loop {
        box_next(&mut it, list, blobs);
        blob_count += 1;
        if it.cycled_list(list) {
            break;
        }
    }
    let _ = blob_count;
    let mut occupation = 0i32;
    let (word_sync, seg_list) = check_pitch_sync3(
        projection_left,
        projection_right,
        0,
        initial_pitch as i16 as i32,
        2,
        projection,
        (f64::from(row.xheight) * TEXTORD_PROJECTION_SCALE) as f32,
        &mut occupation,
    );
    let base = usize::from(!row_cells.is_empty());
    let mut mid_cuts = 0;
    let n = seg_list.len();
    for (k, seg) in seg_list.iter().enumerate() {
        row_cells.insert(base + k, seg.xpos);
        if k + 1 == n {
            mid_cuts = seg.mid_cuts;
        }
    }
    let sd = if occupation > 0 {
        (word_sync / f64::from(occupation)).sqrt() as f32
    } else {
        initial_pitch * 10.0
    };
    (sd, occupation as f32, mid_cuts)
}

#[derive(Clone, Copy, Default)]
struct CutPt {
    faked: bool,
    terminal: bool,
    fake_count: i32,
    region_index: i32,
    mid_cuts: i32,
    xpos: i32,
    back_balance: u32,
    fwd_balance: u32,
    pred: Option<usize>,
    mean_sum: f64,
    sq_sum: f64,
    cost: f64,
}

fn half_pitch_flag(pitch: i32) -> (i32, u32) {
    let half_pitch = (pitch / 2 - 1).clamp(0, 31);
    (half_pitch, 1u32 << half_pitch)
}

fn cut_setup(
    cutpts: &mut [CutPt],
    array_origin: i32,
    projection: &Stats,
    zero_count: i32,
    pitch: i32,
    x: i32,
    offset: i32,
) {
    let (half_pitch, lead_flag) = half_pitch_flag(pitch);
    let mut c = CutPt {
        pred: None,
        mean_sum: 0.0,
        sq_sum: f64::from(offset * offset),
        faked: false,
        terminal: false,
        fake_count: 0,
        xpos: x,
        region_index: 0,
        mid_cuts: 0,
        ..CutPt::default()
    };
    c.cost = c.sq_sum;
    if x == array_origin {
        c.back_balance = 0;
        c.fwd_balance = 0;
        for ind in 0..=half_pitch {
            c.fwd_balance >>= 1;
            if projection.pile_count(ind) > zero_count {
                c.fwd_balance |= lead_flag;
            }
        }
    } else {
        let prev = cutpts[(x - 1 - array_origin) as usize];
        c.back_balance = prev.back_balance << 1;
        c.back_balance &= lead_flag.wrapping_add(lead_flag.wrapping_sub(1));
        if projection.pile_count(x) > zero_count {
            c.back_balance |= 1;
        }
        c.fwd_balance = prev.fwd_balance >> 1;
        if projection.pile_count(x + half_pitch) > zero_count {
            c.fwd_balance |= lead_flag;
        }
    }
    cutpts[(x - array_origin) as usize] = c;
}

#[allow(clippy::too_many_arguments)]
fn cut_assign(
    cutpts: &mut [CutPt],
    array_origin: i32,
    x: i32,
    faking: bool,
    mid_cut: bool,
    offset: i32,
    projection: &Stats,
    projection_scale: f32,
    zero_count: i32,
    pitch: i32,
    pitch_error: i32,
) {
    let (half_pitch, lead_flag) = half_pitch_flag(pitch);
    let prev = cutpts[(x - 1 - array_origin) as usize];
    let mut c = cutpts[(x - array_origin) as usize];
    c.back_balance = prev.back_balance << 1;
    c.back_balance &= lead_flag.wrapping_add(lead_flag.wrapping_sub(1));
    if projection.pile_count(x) > zero_count {
        c.back_balance |= 1;
    }
    c.fwd_balance = prev.fwd_balance >> 1;
    if projection.pile_count(x + half_pitch) > zero_count {
        c.fwd_balance |= lead_flag;
    }
    c.xpos = x;
    c.cost = f64::from(f32::MAX);
    c.pred = None;
    c.faked = faking;
    c.terminal = false;
    c.region_index = 0;
    c.fake_count = i32::from(i16::MAX);
    for index in x - pitch - pitch_error..=x - pitch + pitch_error {
        if index >= array_origin {
            let segpt = cutpts[(index - array_origin) as usize];
            let dist = x - segpt.xpos;
            if !segpt.terminal && segpt.fake_count < i32::from(i16::MAX) {
                let mut balance_count: i16 = 0;
                let mut bi = 0;
                while index + bi < x - bi {
                    let a = projection.pile_count(index + bi) <= zero_count;
                    let b = projection.pile_count(x - bi) <= zero_count;
                    balance_count = balance_count.wrapping_add(i16::from(a ^ b));
                    bi += 1;
                }
                balance_count = (f64::from(balance_count) * TEXTORD_BALANCE_FACTOR
                    / f64::from(projection_scale)) as i16;
                let r_index = segpt.region_index + 1;
                let total = segpt.mean_sum + f64::from(dist);
                balance_count = balance_count.wrapping_add(offset as i16);
                let bc = i32::from(balance_count);
                let sq_dist = f64::from(dist * dist) + segpt.sq_sum + f64::from(bc * bc);
                let mean = total / f64::from(r_index);
                let mut factor = mean - f64::from(pitch);
                factor *= factor;
                factor += sq_dist / f64::from(r_index) - mean * mean;
                if factor < c.cost && segpt.fake_count + i32::from(faking) <= c.fake_count {
                    c.cost = factor;
                    c.pred = Some((index - array_origin) as usize);
                    c.mean_sum = total;
                    c.sq_sum = sq_dist;
                    c.fake_count = segpt.fake_count + i32::from(faking);
                    c.mid_cuts = segpt.mid_cuts + i32::from(mid_cut);
                    c.region_index = r_index;
                }
            }
        }
    }
    cutpts[(x - array_origin) as usize] = c;
}

struct SegPt {
    xpos: i32,
    mid_cuts: i32,
}

/// `check_pitch_sync3` (via `check_pitch_sync2`, which only clamps the
/// pitch error first): returns (sync measure, cut points left to right).
#[allow(clippy::too_many_arguments)]
fn check_pitch_sync3(
    projection_left: i32,
    projection_right: i32,
    zero_count: i32,
    mut pitch: i32,
    mut pitch_error: i32,
    projection: &Stats,
    projection_scale: f32,
    occupation_count: &mut i32,
) -> (f64, Vec<SegPt>) {
    if pitch < 3 {
        pitch = 3;
    }
    if (pitch - 3) / 2 < pitch_error {
        pitch_error = (pitch - 3) / 2;
    }
    // check_pitch_sync3 repeats the clamp.
    let zero_offset = (f64::from(pitch) * PITSYNC_JOINED_EDGE) as i16 as i32;
    let mut left_edge = projection_left;
    while projection.pile_count(left_edge) == 0 && left_edge < projection_right {
        left_edge += 1;
    }
    let mut right_edge = projection_right;
    while projection.pile_count(right_edge) == 0 && right_edge > left_edge {
        right_edge -= 1;
    }
    let array_origin = left_edge - pitch;
    let mut cutpts = vec![CutPt::default(); (right_edge - left_edge + pitch * 2 + 1) as usize];
    let mut mins = vec![false; (pitch_error * 2 + 1) as usize];
    let mut x = array_origin;
    while x < left_edge {
        cut_setup(
            &mut cutpts,
            array_origin,
            projection,
            zero_count,
            pitch,
            x,
            0,
        );
        x += 1;
    }
    let mut prev_zero = left_edge - 1;
    for offset in 0..=pitch_error {
        cut_setup(
            &mut cutpts,
            array_origin,
            projection,
            zero_count,
            pitch,
            x,
            offset,
        );
        x += 1;
    }
    let mut minindex = 0usize;
    for offset in -pitch_error..pitch_error {
        mins[minindex] = projection.local_min(x + offset);
        minindex += 1;
    }
    let mut next_zero = x + zero_offset + 1;
    let mut offset = next_zero - 1;
    while offset >= x {
        if projection.pile_count(offset) <= zero_count {
            next_zero = offset;
            break;
        }
        offset -= 1;
    }
    let pe2 = (pitch_error * 2) as usize;
    while x < right_edge - pitch_error {
        mins[minindex] = projection.local_min(x + pitch_error);
        minindex += 1;
        if minindex > pe2 {
            minindex = 0;
        }
        let mut faking = false;
        let mut mid_cut = false;
        let mut offset = 0;
        if projection.pile_count(x) <= zero_count {
            prev_zero = x;
        } else {
            offset = 1;
            while offset <= pitch_error {
                if projection.pile_count(x + offset) <= zero_count
                    || projection.pile_count(x - offset) <= zero_count
                {
                    break;
                }
                offset += 1;
            }
        }
        if offset > pitch_error {
            if x - prev_zero > zero_offset && next_zero - x > zero_offset {
                offset = 0;
                while offset <= pitch_error {
                    let mut test_index = minindex + pitch_error as usize + offset as usize;
                    if test_index > pe2 {
                        test_index -= pe2 + 1;
                    }
                    if mins[test_index] {
                        break;
                    }
                    let mut test_index = minindex as i32 + pitch_error - offset;
                    if test_index > pe2 as i32 {
                        test_index -= pe2 as i32 + 1;
                    }
                    if mins[test_index as usize] {
                        break;
                    }
                    offset += 1;
                }
            }
            if offset > pitch_error {
                offset = projection.pile_count(x);
                faking = true;
            } else {
                let projection_offset =
                    (projection.pile_count(x) as f32 / projection_scale) as i16 as i32;
                if projection_offset > offset {
                    offset = projection_offset;
                }
                mid_cut = true;
            }
        }
        cut_assign(
            &mut cutpts,
            array_origin,
            x,
            faking,
            mid_cut,
            offset,
            projection,
            projection_scale,
            zero_count,
            pitch,
            pitch_error,
        );
        x += 1;
        if next_zero < x || next_zero == x + zero_offset {
            next_zero = x + zero_offset + 1;
        }
        if projection.pile_count(x + zero_offset) <= zero_count {
            next_zero = x + zero_offset;
        }
    }
    let mut best_fake = i32::from(i16::MAX);
    let mut best_cost = f64::from(i32::MAX);
    let mut best_count = i32::from(i16::MAX);
    let mut best_left_x = 0;
    let mut best_right_x = 0;
    while x < right_edge + pitch {
        let offset = if x < right_edge { right_edge - x } else { 0 };
        cut_assign(
            &mut cutpts,
            array_origin,
            x,
            false,
            false,
            offset,
            projection,
            projection_scale,
            zero_count,
            pitch,
            pitch_error,
        );
        let c = &mut cutpts[(x - array_origin) as usize];
        c.terminal = true;
        let c = *c;
        if c.region_index + c.fake_count <= best_count + best_fake {
            if c.fake_count < best_fake || (c.fake_count == best_fake && c.cost < best_cost) {
                best_fake = c.fake_count;
                best_cost = c.cost;
                best_left_x = x;
                best_right_x = x;
                best_count = c.region_index;
            } else if c.fake_count == best_fake && x == best_right_x + 1 && c.cost == best_cost {
                best_right_x = x;
            }
        }
        x += 1;
    }
    let mut best_end = Some(((best_left_x + best_right_x) / 2 - array_origin) as usize);
    *occupation_count = -1;
    let mut segs: Vec<SegPt> = Vec::new();
    let mut last_sum = 0.0;
    let mut last_sq = 0.0;
    let mut first = true;
    while let Some(e) = best_end {
        let c = cutpts[e];
        let mut x = c.xpos - pitch + pitch_error;
        while x < c.xpos - pitch_error && projection.pile_count(x) == 0 {
            x += 1;
        }
        if x < c.xpos - pitch_error {
            *occupation_count += 1;
        }
        if first {
            last_sum = c.mean_sum;
            last_sq = c.sq_sum;
            first = false;
        }
        segs.push(SegPt {
            xpos: c.xpos,
            mid_cuts: c.mid_cuts,
        });
        best_end = c.pred;
    }
    segs.reverse();
    let mean_sum = last_sum * last_sum / f64::from(best_count);
    (last_sq - mean_sum, segs)
}

/// `find_repeated_chars`.
fn find_repeated_chars(block: &mut ToBlk, blobs: &mut Blobs) {
    if !block.is_text() {
        return;
    }
    for row in &mut block.rows {
        if row.blobs.is_empty() {
            continue;
        }
        if !row.rep_chars_marked() {
            mark_repeated(row, blobs);
        }
        if row.num_repeated_sets == 0 {
            continue;
        }
        let list = &mut row.blobs;
        let mut box_it = Iter::new(list);
        loop {
            let b = blobs.get(box_it.data(list));
            if b.repeated_set != 0 && !b.joined {
                let set = b.repeated_set;
                let mut blobcount = 1;
                let mut search_it = box_it;
                search_it.forward(list);
                while !search_it.at_first(list)
                    && blobs.get(search_it.data(list)).repeated_set == set
                {
                    blobcount += 1;
                    search_it.forward(list);
                }
                let bol = box_it.at_first(list);
                let mut word = make_real_word(&mut box_it, list, blobcount, bol, 1, blobs);
                word.set_flag(W_REP_CHAR, true);
                word.set_flag(W_DONT_CHOP, true);
                row.rep_words.push(word);
            } else {
                box_it.forward(list);
            }
            if box_it.at_first(list) {
                break;
            }
        }
    }
}

/// `make_real_word`.
fn make_real_word(
    box_it: &mut Iter,
    list: &mut EList<BlobId>,
    blobcount: i32,
    bol: bool,
    mut blanks: u8,
    blobs: &mut Blobs,
) -> Werd {
    let mut cblobs: Vec<CBlob> = Vec::new();
    for _ in 0..blobcount {
        let id = box_it.extract(list);
        let b = blobs.get_mut(id);
        let joined = b.joined;
        if let Some(cb) = b.cblob.take() {
            if joined {
                if let Some(last) = cblobs.last_mut() {
                    last.outlines.extend(cb.outlines);
                }
            } else {
                cblobs.push(cb);
            }
        }
        box_it.forward(list);
    }
    if blanks < 1 {
        blanks = 1;
    }
    let mut word = Werd::new(cblobs, blanks);
    if bol {
        word.set_flag(W_BOL, true);
    }
    if box_it.at_first(list) {
        word.set_flag(W_EOL, true);
    }
    word
}

// ---------------------------------------------------------------- tospace

/// `GAPMAP`.
struct GapMap {
    min_left: i32,
    bucket_size: i32,
    map_max: i32,
    total_rows: i32,
    any_tabs: bool,
    map: Vec<i32>,
}

impl GapMap {
    fn new(block: &ToBlk, blobs: &Blobs) -> GapMap {
        let mut xht_stats = Stats::new(0, 127);
        let mut min_left = i32::from(i16::MAX);
        let mut max_right = -i32::from(i16::MAX);
        let mut total_rows = 0;
        for row in &block.rows {
            if !row.blobs.is_empty() {
                total_rows += 1;
                xht_stats.add((f64::from(row.xheight) + 0.5).floor() as i16 as i32, 1);
                let v = row.blobs.to_vec();
                let start = blobs.get(v[0]).bbox.left;
                let end = blobs.get(v[v.len() - 1]).bbox.right;
                min_left = min_left.min(start);
                max_right = max_right.max(end);
            }
        }
        let mut g = GapMap {
            min_left: 0,
            bucket_size: 0,
            map_max: 0,
            total_rows: 0,
            any_tabs: false,
            map: Vec::new(),
        };
        if total_rows < 3 || min_left >= max_right {
            return g;
        }
        g.min_left = min_left;
        g.total_rows = total_rows;
        g.bucket_size = i32::from(((xht_stats.median() + 0.5).floor() as i16) / 2);
        g.map_max = (max_right - min_left) / g.bucket_size;
        g.map = vec![0; (g.map_max + 1) as usize];
        for row in &block.rows {
            if row.blobs.is_empty() {
                continue;
            }
            let list = &row.blobs;
            let mut it = Iter::new(list);
            it.mark_cycle_pt();
            let mut prev = box_next(&mut it, list, blobs);
            while !it.cycled_list(list) {
                let bx = box_next(&mut it, list, blobs);
                let gap_width = bx.left - prev.right;
                if f64::from(gap_width) > GAPMAP_BIG_GAPS * f64::from(row.xheight) && gap_width > 2
                {
                    let min_q = (prev.right - min_left) / g.bucket_size;
                    let max_q = ((bx.left - min_left) / g.bucket_size).min(g.map_max);
                    for i in min_q..=max_q {
                        g.map[i as usize] += 1;
                    }
                }
                prev = bx;
            }
        }
        for i in 0..=g.map_max {
            if g.map[i as usize] > total_rows / 2 {
                g.any_tabs = true;
            }
        }
        g
    }

    fn table_gap(&self, left: i32, right: i32) -> bool {
        if !self.any_tabs {
            return false;
        }
        let min_q = ((left - self.min_left) / self.bucket_size).max(0);
        let max_q = ((right - self.min_left) / self.bucket_size).min(self.map_max);
        (min_q..=max_q).any(|i| self.map[i as usize] > self.total_rows / 2)
    }
}

fn to_spacing(blocks: &mut [ToBlk], blobs: &mut Blobs) {
    for block in blocks.iter_mut() {
        let gapmap = GapMap::new(block, blobs);
        let (space_w, non_space_w) = block_spacing_stats(block, &gapmap, blobs);
        for row in &mut block.rows {
            if row.pitch_decision == PitchType::DefProp || row.pitch_decision == PitchType::CorrProp
            {
                row_spacing_stats(row, &gapmap, space_w, non_space_w, blobs);
            }
        }
    }
}

fn is_prop(row: &ToRow) -> bool {
    row.pitch_decision == PitchType::DefProp || row.pitch_decision == PitchType::CorrProp
}

/// Gaps between `reduced_box_next` boxes along a row: (row_length, gaps as
/// (prev box, box)). `length_from_end` picks how row_length is measured.
fn row_gaps(row: &ToRow, blobs: &mut Blobs, length_from_end: bool) -> (i32, Vec<(TBox, TBox)>) {
    let list = &row.blobs;
    let mut it = Iter::new(list);
    it.mark_cycle_pt();
    let end_of_row = blobs.get(it.data_relative(list, -1)).bbox.right;
    let first = reduced_box_next(row, &mut it, blobs);
    let row_length = if length_from_end {
        end_of_row - first.left
    } else {
        first.left - end_of_row
    };
    let mut prev = first;
    let mut gaps = Vec::new();
    while !it.cycled_list(list) {
        let bx = reduced_box_next(row, &mut it, blobs);
        gaps.push((prev, bx));
        prev = bx;
    }
    (row_length, gaps)
}

/// `block_spacing_stats`: returns (space gap width, non-space gap width).
fn block_spacing_stats(block: &ToBlk, gapmap: &GapMap, blobs: &mut Blobs) -> (i32, i32) {
    let mut centre_to_centre_stats = Stats::new(0, MAXSPACING - 1);
    let mut all_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut space_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut minwidth = MAXSPACING;
    for row in &block.rows {
        if row.blobs.is_empty() || !is_prop(row) {
            continue;
        }
        let (row_length, gaps) = row_gaps(row, blobs, true);
        if let Some((first, _)) = gaps.first() {
            minwidth = minwidth.min(first.width());
        } else {
            // A single blob: its box is still the first reduced box.
            let list = &row.blobs;
            let mut it = Iter::new(list);
            it.mark_cycle_pt();
            let b = reduced_box_next(row, &mut it, blobs);
            minwidth = minwidth.min(b.width());
        }
        for (prev, bx) in gaps {
            minwidth = minwidth.min(bx.width());
            let left = i32::from(prev.right as i16);
            let right = i32::from(bx.left as i16);
            let gap_width = right - left;
            if !ignore_big_gap(row, row_length, gapmap, left, right) {
                all_gap_stats.add(gap_width, 1);
                let c2c = (right + bx.right - (prev.left + left)) / 2;
                centre_to_centre_stats.add(i32::from(c2c as i16), 1);
            }
        }
    }
    if all_gap_stats.get_total() <= 1 {
        return (-1, i32::from(minwidth as i16));
    }
    let non_space = i32::from(all_gap_stats.median().floor() as i16);
    for row in &block.rows {
        if row.blobs.is_empty() || !is_prop(row) {
            continue;
        }
        let real_space_threshold = (TOSP_INIT_GUESS_KN_MULT * f64::from(non_space))
            .max(TOSP_INIT_GUESS_XHT_MULT * f64::from(row.xheight))
            as f32;
        let (row_length, gaps) = row_gaps(row, blobs, false);
        for (prev, bx) in gaps {
            let left = i32::from(prev.right as i16);
            let right = i32::from(bx.left as i16);
            let gap_width = right - left;
            if gap_width as f32 > real_space_threshold
                && !ignore_big_gap(row, row_length, gapmap, left, right)
                && cert_space(row, gap_width, &prev, &bx)
            {
                space_gap_stats.add(gap_width, 1);
            }
        }
    }
    let space = if space_gap_stats.get_total() <= 2 {
        -1
    } else {
        i32::from((space_gap_stats.median().floor() as i16).max((3 * non_space) as i16))
    };
    (space, non_space)
}

/// The "obvious space" test shared by the spacing statistics.
fn cert_space(row: &ToRow, gap_width: i32, prev: &TBox, bx: &TBox) -> bool {
    let xh = f64::from(row.xheight);
    f64::from(gap_width) > TOSP_FUZZY_SPACE_FACTOR2 * xh
        || (f64::from(gap_width) > TOSP_FUZZY_SPACE_FACTOR1 * xh
            && !narrow_blob(row, prev)
            && !narrow_blob(row, bx))
        || (wide_blob(row, prev) && wide_blob(row, bx))
}

fn row_spacing_stats(
    row: &mut ToRow,
    gapmap: &GapMap,
    mut block_space_gap_width: i32,
    block_non_space_gap_width: i32,
    blobs: &mut Blobs,
) {
    let mut all_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut cert_space_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut all_space_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut small_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut large_gap_count = 0;
    let good_block_space_estimate = block_space_gap_width > 0;
    if !good_block_space_estimate {
        block_space_gap_width = i32::from((row.xheight / 2.0).floor() as i16);
    }
    if !row.blobs.is_empty() {
        let real_space_threshold =
            i32::from(((block_space_gap_width + block_non_space_gap_width) / 2) as i16);
        let (row_length, gaps) = row_gaps(row, blobs, true);
        for (prev, bx) in gaps {
            let left = i32::from(prev.right as i16);
            let right = i32::from(bx.left as i16);
            let gap_width = i32::from((right - left) as i16);
            if ignore_big_gap(row, row_length, gapmap, left, right) {
                large_gap_count += 1;
            } else {
                if gap_width >= real_space_threshold {
                    if cert_space(row, gap_width, &prev, &bx) {
                        cert_space_gap_stats.add(gap_width, 1);
                    }
                    all_space_gap_stats.add(gap_width, 1);
                } else {
                    small_gap_stats.add(gap_width, 1);
                }
                all_gap_stats.add(gap_width, 1);
            }
        }
    }
    let suspected_table = large_gap_count > 1
        || (large_gap_count > 0 && all_gap_stats.get_total() <= TOSP_FEW_SAMPLES);
    if cert_space_gap_stats.get_total() >= TOSP_ENOUGH_SPACE_SAMPLES_FOR_MEDIAN
        || ((suspected_table || all_gap_stats.get_total() <= TOSP_SHORT_ROW)
            && cert_space_gap_stats.get_total() > 0)
    {
        old_to_method(
            row,
            &all_gap_stats,
            &cert_space_gap_stats,
            block_space_gap_width,
            block_non_space_gap_width,
        );
    } else if !isolated_row_stats(row, gapmap, &all_gap_stats, suspected_table, blobs) {
        if good_block_space_estimate {
            row.space_size = block_space_gap_width as f32;
            if all_gap_stats.get_total() > TOSP_REDO_KERN_LIMIT {
                row.kern_size = all_gap_stats.median() as f32;
            } else {
                row.kern_size = block_non_space_gap_width as f32;
            }
            row.space_threshold = (f64::from(row.space_size + row.kern_size)
                / TOSP_OLD_SP_KN_TH_FACTOR)
                .floor() as i32;
        } else {
            old_to_method(
                row,
                &all_gap_stats,
                &all_space_gap_stats,
                block_space_gap_width,
                block_non_space_gap_width,
            );
        }
    }
    // tosp_sanity_method == 1
    let kern_floor = f64::from(row.kern_size.max(2.5));
    let xh = f64::from(row.xheight);
    if f64::from(row.space_size) < TOSP_MIN_SANE_KN_SP * kern_floor
        || f64::from(row.space_size - row.kern_size) < TOSP_SILLY_KN_SP_GAP * xh
    {
        let sane_space = if good_block_space_estimate
            && f64::from(block_space_gap_width) >= TOSP_MIN_SANE_KN_SP * f64::from(row.kern_size)
        {
            block_space_gap_width as f32
        } else {
            ((TOSP_MIN_SANE_KN_SP as f32) * row.kern_size.max(2.5)).max(row.xheight / 2.0)
        };
        row.space_size = sane_space;
        row.space_threshold =
            (f64::from(row.space_size + row.kern_size) / TOSP_OLD_SP_KN_TH_FACTOR).floor() as i32;
    }
    let sane_threshold =
        (TOSP_MAX_SANE_KN_THRESH * f64::from(row.kern_size.max(2.5))).floor() as i32;
    if row.space_threshold > sane_threshold {
        row.space_threshold = sane_threshold;
        if row.space_size <= sane_threshold as f32 {
            row.space_size = row.space_threshold as f32 + 1.0;
        }
    }
    if suspected_table {
        let sane_space = (TOSP_TABLE_KN_SP_RATIO * f64::from(row.kern_size))
            .max(TOSP_TABLE_XHT_SP_RATIO * f64::from(row.xheight)) as f32;
        let sane_threshold = ((sane_space + row.kern_size) / 2.0).floor() as i32;
        if row.space_size < sane_space || row.space_threshold < sane_threshold {
            row.space_threshold = sane_space as i32;
            row.space_size = (row.space_threshold as f32 + 1.0).max(row.xheight);
        }
    }
    // !tosp_old_to_method
    row.min_space = ((TOSP_FUZZY_SPACE_FACTOR * f64::from(row.xheight)).ceil() as i32)
        .min(row.space_size as i32);
    if row.min_space <= row.space_threshold {
        row.min_space = row.space_threshold + 1;
    }
    let max_max_nonspace = ((row.space_threshold as f32 + row.kern_size) / 2.0) as i32;
    row.max_nonspace = max_max_nonspace;
    let mut max = 0i16;
    for index in 0..=max_max_nonspace {
        let pc = all_gap_stats.pile_count(index);
        if pc > i32::from(max) {
            max = pc as i16;
        }
        if index as f32 > row.kern_size && f64::from(pc) < 0.1 * f64::from(max) {
            row.max_nonspace = index;
            break;
        }
    }
    if row.space_size > row.space_threshold as f32 {
        row.min_space = row.min_space.max(
            (f64::from(row.space_threshold)
                + TOSP_FUZZY_SP_FRACTION * f64::from(row.space_size - row.space_threshold as f32))
            .ceil() as i32,
        );
    }
    // tosp_table_fuzzy_kn_sp_ratio > 0 && tosp_fuzzy_limit_all
    row.min_space = row
        .min_space
        .max((TOSP_TABLE_FUZZY_KN_SP_RATIO * f64::from(row.kern_size)).ceil() as i32);
    if row.kern_size < row.space_threshold as f32 {
        row.max_nonspace = (0.5
            + f64::from(row.kern_size)
            + TOSP_FUZZY_KN_FRACTION * f64::from(row.space_threshold as f32 - row.kern_size))
        .floor() as i32;
    }
    if row.max_nonspace > row.space_threshold {
        row.max_nonspace = row.space_threshold;
    }
}

fn old_to_method(
    row: &mut ToRow,
    all_gap_stats: &Stats,
    space_gap_stats: &Stats,
    block_space_gap_width: i32,
    block_non_space_gap_width: i32,
) {
    if space_gap_stats.get_total() >= TOSP_ENOUGH_SPACE_SAMPLES_FOR_MEDIAN {
        row.space_size = space_gap_stats.median() as f32;
        if f64::from(row.space_size) > f64::from(block_space_gap_width) * 1.5 {
            row.space_size = block_space_gap_width as f32;
        }
        if row.space_size < (block_non_space_gap_width * 2 + 1) as f32 {
            row.space_size = (block_non_space_gap_width * 2 + 1) as f32;
        }
    } else if space_gap_stats.get_total() >= 1 {
        row.space_size = space_gap_stats.mean() as f32;
        if f64::from(row.space_size) > f64::from(block_space_gap_width) * 1.5 {
            row.space_size = block_space_gap_width as f32;
        }
        if row.space_size < (block_non_space_gap_width * 3 + 1) as f32 {
            row.space_size = (block_non_space_gap_width * 3 + 1) as f32;
        }
    } else {
        row.space_size = block_space_gap_width as f32;
    }
    if all_gap_stats.get_total() > TOSP_REDO_KERN_LIMIT {
        row.kern_size = all_gap_stats.median() as f32;
    } else {
        row.kern_size = block_non_space_gap_width as f32;
    }
    row.space_threshold = ((row.space_size + row.kern_size) / 2.0).floor() as i32;
}

fn isolated_row_stats(
    row: &mut ToRow,
    gapmap: &GapMap,
    all_gap_stats: &Stats,
    suspected_table: bool,
    blobs: &mut Blobs,
) -> bool {
    let kern_estimate = all_gap_stats.median() as f32;
    let crude_threshold_estimate = (TOSP_INIT_GUESS_KN_MULT * f64::from(kern_estimate))
        .max(TOSP_INIT_GUESS_XHT_MULT * f64::from(row.xheight))
        as f32;
    let small_gaps_count = i32::from(stats_count_under(
        all_gap_stats,
        crude_threshold_estimate.ceil() as i16 as i32,
    ));
    let total = i32::from(all_gap_stats.get_total() as i16);
    if total <= TOSP_REDO_KERN_LIMIT
        || f64::from(small_gaps_count as f32 / total as f32) < TOSP_ENOUGH_SMALL_GAPS
        || total - small_gaps_count < 1
    {
        return false;
    }
    let mut cert_space_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut all_space_gap_stats = Stats::new(0, MAXSPACING - 1);
    let mut small_gap_stats = Stats::new(0, MAXSPACING - 1);
    let (row_length, gaps) = row_gaps(row, blobs, true);
    for (prev, bx) in gaps {
        let left = i32::from(prev.right as i16);
        let right = i32::from(bx.left as i16);
        let gap_width = i32::from((right - left) as i16);
        if !ignore_big_gap(row, row_length, gapmap, left, right)
            && gap_width as f32 > crude_threshold_estimate
        {
            if cert_space(row, gap_width, &prev, &bx) {
                cert_space_gap_stats.add(gap_width, 1);
            }
            all_space_gap_stats.add(gap_width, 1);
        }
        if (gap_width as f32) < crude_threshold_estimate {
            small_gap_stats.add(gap_width, 1);
        }
    }
    if cert_space_gap_stats.get_total() >= TOSP_ENOUGH_SPACE_SAMPLES_FOR_MEDIAN {
        row.space_size = cert_space_gap_stats.median() as f32;
    } else if suspected_table && cert_space_gap_stats.get_total() > 0 {
        row.space_size = cert_space_gap_stats.mean() as f32;
    } else if all_space_gap_stats.get_total() >= TOSP_ENOUGH_SPACE_SAMPLES_FOR_MEDIAN {
        row.space_size = all_space_gap_stats.median() as f32;
    } else {
        row.space_size = all_space_gap_stats.mean() as f32;
    }
    row.kern_size = all_gap_stats.median() as f32;
    row.space_threshold = ((row.space_size + row.kern_size) / 2.0).floor() as i32;
    if row.kern_size >= row.space_threshold as f32
        || row.space_threshold as f32 >= row.space_size
        || row.space_threshold <= 0
    {
        row.kern_size = 0.0;
        row.space_threshold = 0;
        row.space_size = 0.0;
        return false;
    }
    true
}

fn stats_count_under(stats: &Stats, threshold: i32) -> i16 {
    let mut total: i16 = 0;
    for index in 0..threshold {
        total = total.wrapping_add(stats.pile_count(index) as i16);
    }
    total
}

fn ignore_big_gap(row: &ToRow, _row_length: i32, gapmap: &GapMap, left: i32, right: i32) -> bool {
    let gap = f64::from(right - left + 1);
    let xh = f64::from(row.xheight);
    // tosp_ignore_big_gaps == -1
    if gap > TOSP_IGNORE_VERY_BIG_GAPS * xh {
        return true;
    }
    gap > GAPMAP_BIG_GAPS * xh && gapmap.table_gap(left, right)
}

fn narrow_blob(row: &ToRow, b: &TBox) -> bool {
    f64::from(b.width()) <= TOSP_NARROW_FRACTION * f64::from(row.xheight)
        || f64::from(b.width() as f32 / b.height() as f32) <= TOSP_NARROW_ASPECT_RATIO
}

fn wide_blob(row: &ToRow, b: &TBox) -> bool {
    f64::from(b.width()) >= TOSP_WIDE_FRACTION * f64::from(row.xheight)
}

fn suspected_punct_blob(row: &ToRow, b: &TBox) -> bool {
    let blob_x_centre = (f64::from(b.right + b.left) / 2.0) as f32;
    let baseline = row.baseline.y(f64::from(blob_x_centre)) as f32;
    let xh = f64::from(row.xheight);
    f64::from(b.height()) <= 0.66 * xh
        || f64::from(b.top) < f64::from(baseline) + xh / 2.0
        || f64::from(b.bottom) > f64::from(baseline) + xh / 2.0
}

/// `reduced_box_next`.
fn reduced_box_next(row: &ToRow, it: &mut Iter, blobs: &mut Blobs) -> TBox {
    let list = &row.blobs;
    let head = it.data(list);
    if blobs.get(head).reduced {
        let reduced_box = blobs.get(head).red_box;
        loop {
            it.forward(list);
            let b = blobs.get(it.data(list));
            if !(b.cblob.is_none() || b.joined) {
                break;
            }
        }
        return reduced_box;
    }
    let mut full_box = blobs.get(head).bbox;
    let (mut reduced_box, mut left_above_xht) = reduced_box_for_blob(head, row, blobs);
    loop {
        it.forward(list);
        let id = it.data(list);
        let b = blobs.get(id);
        if b.cblob.is_none() {
            full_box.union_with(&b.bbox);
        } else if b.joined {
            let (rb, l) = reduced_box_for_blob(id, row, blobs);
            reduced_box.union_with(&rb);
            left_above_xht = left_above_xht.min(l);
        }
        let b = blobs.get(id);
        if !(b.cblob.is_none() || b.joined) {
            break;
        }
    }
    if !(reduced_box.width() > 0
        && (f64::from(reduced_box.left) + TOSP_NEAR_LH_EDGE * f64::from(reduced_box.width()))
            < f64::from(left_above_xht)
        && f64::from(reduced_box.height()) > 0.7 * f64::from(row.xheight))
    {
        reduced_box = full_box;
    }
    let h = blobs.get_mut(head);
    h.red_box = reduced_box;
    h.reduced = true;
    reduced_box
}

/// `reduced_box_for_blob`: (box, left edge above the x-height).
fn reduced_box_for_blob(id: BlobId, row: &ToRow, blobs: &Blobs) -> (TBox, i32) {
    let b = blobs.get(id);
    let blob_box = b.bbox;
    let cb = b.cblob.as_ref().expect("reduced box of a real blob");
    let blob_x_centre = (f64::from(blob_box.left + blob_box.right) / 2.0) as f32;
    let baseline = row.baseline.y(f64::from(blob_x_centre)) as f32;
    let (left_limit, junk) = find_cblob_hlimits(
        cb,
        (f64::from(baseline) + 1.1 * f64::from(row.xheight)) as f32,
        f32::from(i16::MAX),
    );
    let left_above_xht = if left_limit > junk {
        i32::from(i16::MAX)
    } else {
        i32::from(left_limit.floor() as i16)
    };
    let (left_limit, junk) = find_cblob_hlimits(cb, baseline, f32::from(i16::MAX));
    if left_limit > junk {
        return (TBox::default(), left_above_xht);
    }
    let (junk, right_limit) = find_cblob_hlimits(cb, -f32::from(i16::MAX), baseline + row.xheight);
    if junk > right_limit {
        return (TBox::default(), left_above_xht);
    }
    (
        TBox::from_corners(
            ICoord::new(i32::from(left_limit.floor() as i16), blob_box.bottom),
            ICoord::new(i32::from(right_limit.ceil() as i16), blob_box.top),
        ),
        left_above_xht,
    )
}

// ---------------------------------------------------------------- wordseg

fn make_real_words(block: &mut ToBlk, blobs: &mut Blobs) -> TextBlock {
    let mut rows = Vec::new();
    let is_text = block.is_text();
    let (bkern, bspace) = (block.kern_size as i16, block.space_size as i16);
    let bxh = block.xheight;
    for row in &mut block.rows {
        let real_row = if row.blobs.is_empty() && !row.rep_words.is_empty() {
            make_rep_words(row, bxh, bkern, bspace)
        } else if !row.blobs.is_empty() {
            if !is_text
                || row.pitch_decision == PitchType::DefProp
                || row.pitch_decision == PitchType::CorrProp
            {
                make_prop_words(row, blobs)
            } else {
                fixed_pitch_words(row, blobs)
            }
        } else {
            None
        };
        if let Some(r) = real_row {
            rows.push(r);
        }
    }
    TextBlock {
        bbox: block.bbox,
        ptype: block.ptype,
        rows,
        xheight: block.block_xheight,
        kerning: block.kern_size as i16,
        spacing: block.space_size as i16,
        pitch: block.fixed_pitch as i16,
        proportional: block.fixed_pitch == 0.0,
    }
}

fn make_rep_words(row: &mut ToRow, block_xheight: f32, kern: i16, space: i16) -> Option<PageRow> {
    if row.rep_words.is_empty() {
        return None;
    }
    row.xheight = block_xheight;
    let mut real_row = PageRow::new(row, kern, space);
    real_row.words = std::mem::take(&mut row.rep_words);
    real_row.recalc_bounding_box();
    Some(real_row)
}

fn find_mean_blob_spacing(word: &Werd) -> f32 {
    let mut gap_sum = 0i32;
    let mut gap_count = 0i16;
    if let Some(first) = word.cblobs.first() {
        let mut prev_right = first.bounding_box().right;
        for b in &word.cblobs[1..] {
            let bb = b.bounding_box();
            gap_sum += bb.left - prev_right;
            gap_count += 1;
            prev_right = bb.right;
        }
    }
    if gap_count > 0 {
        gap_sum as f32 / f32::from(gap_count)
    } else {
        0.0
    }
}

/// `peek_at_next_gap`: (next box, next gap, next within-x-height gap).
fn peek_at_next_gap(row: &ToRow, box_it: Iter, blobs: &mut Blobs) -> (TBox, i32, i32) {
    let list = &row.blobs;
    let mut box_it = box_it;
    let mut reduced_it = box_it;
    let next_blob_box = box_next(&mut box_it, list, blobs);
    let next_reduced = reduced_box_next(row, &mut reduced_it, blobs);
    if box_it.at_first(list) {
        (next_blob_box, i32::from(i16::MAX), i32::from(i16::MAX))
    } else {
        let beyond = blobs.get(box_it.data(list)).bbox;
        let next_gap = i32::from((beyond.left - next_blob_box.right) as i16);
        let beyond = reduced_box_next(row, &mut reduced_it, blobs);
        let next_within = i32::from((beyond.left - next_reduced.right) as i16);
        (next_blob_box, next_gap, next_within)
    }
}

struct BreakState {
    blanks: u8,
    fuzzy_sp: bool,
    fuzzy_non: bool,
    prev_gap_was_a_space: bool,
}

#[allow(clippy::too_many_arguments)]
fn make_a_word_break(
    row: &ToRow,
    blob_box: &TBox,
    prev_gap: i32,
    prev_blob_box: &TBox,
    real_current_gap: i32,
    mut within_xht_current_gap: i32,
    next_blob_box: &TBox,
    next_gap: i32,
    st: &mut BreakState,
) -> bool {
    let _ = blob_box;
    let xh = f64::from(row.xheight);
    if f64::from(row.kern_size) > TOSP_LARGE_KERNING * xh {
        within_xht_current_gap = real_current_gap;
    }
    let current_gap = real_current_gap;
    if prev_blob_box.null_box() {
        st.prev_gap_was_a_space = true;
    }
    let mut space = current_gap > row.space_threshold;
    let mut num_blanks = current_gap;
    if row.space_size > 1.0 {
        num_blanks = int_cast_rounded_f32(current_gap as f32 / row.space_size);
    }
    st.blanks = num_blanks.clamp(1, 255) as u8;
    st.fuzzy_sp = false;
    st.fuzzy_non = false;
    let narrow_prev = prev_blob_box.width() > 0 && narrow_blob(row, prev_blob_box);
    let narrow_next = next_blob_box.width() > 0 && narrow_blob(row, next_blob_box);
    let cg = f64::from(current_gap);
    if real_current_gap <= row.max_nonspace && within_xht_current_gap > row.max_nonspace {
        space = true;
        st.fuzzy_non = true;
    } else if real_current_gap <= row.space_threshold
        && within_xht_current_gap > row.space_threshold
    {
        space = true;
        st.fuzzy_sp = true;
    } else if real_current_gap < row.min_space && within_xht_current_gap >= row.min_space {
        space = true;
    } else if current_gap < row.min_space && current_gap > row.space_threshold {
        let fuzzy_sp_to_kn_limit = (f64::from(row.kern_size)
            + TOSP_PASS_WIDE_FUZZ_SP_TO_CONTEXT * f64::from(row.space_size - row.kern_size))
            as f32;
        let flip = |space: &mut bool, st: &mut BreakState| {
            if current_gap as f32 > fuzzy_sp_to_kn_limit {
                st.fuzzy_non = true;
            } else {
                *space = false;
            }
        };
        if narrow_prev && st.prev_gap_was_a_space && cg <= TOSP_GAP_FACTOR * f64::from(prev_gap) {
            flip(&mut space, st);
        } else if narrow_prev
            && !st.prev_gap_was_a_space
            && cg * TOSP_GAP_FACTOR <= f64::from(prev_gap)
        {
            flip(&mut space, st);
        } else if narrow_next
            && next_gap > row.space_threshold
            && cg <= TOSP_GAP_FACTOR * f64::from(next_gap)
        {
            flip(&mut space, st);
        } else if narrow_next
            && next_gap <= row.space_threshold
            && cg * TOSP_GAP_FACTOR <= f64::from(next_gap)
        {
            flip(&mut space, st);
        } else if narrow_next || narrow_prev {
            st.fuzzy_sp = true;
        }
    } else if current_gap > row.max_nonspace && current_gap <= row.space_threshold {
        let pn = f64::from(prev_gap.max(next_gap));
        let both = prev_blob_box.width() > 0 && next_blob_box.width() > 0;
        if both
            && cg >= TOSP_KERN_GAP_FACTOR1 * pn
            && wide_blob(row, prev_blob_box)
            && wide_blob(row, next_blob_box)
        {
            space = true;
            st.fuzzy_sp = true;
        } else if both
            && current_gap > 5
            && cg >= TOSP_KERN_GAP_FACTOR2 * pn
            && !(narrow_blob(row, prev_blob_box) || suspected_punct_blob(row, prev_blob_box))
            && !(narrow_blob(row, next_blob_box) || suspected_punct_blob(row, next_blob_box))
        {
            space = true;
            st.fuzzy_non = true;
        } else if both && cg >= TOSP_KERN_GAP_FACTOR3 * pn {
            space = true;
            st.fuzzy_non = true;
        }
    }
    st.prev_gap_was_a_space = space && !st.fuzzy_non;
    space
}

/// `make_prop_words`.
fn make_prop_words(row: &mut ToRow, blobs: &mut Blobs) -> Option<PageRow> {
    if row.blobs.is_empty() {
        return None;
    }
    let mut rep_words: std::collections::VecDeque<Werd> = std::mem::take(&mut row.rep_words).into();
    let mut next_rep_char_word_right = rep_words
        .front()
        .map_or(i32::MAX, |w| w.bounding_box().right);
    let mut words: Vec<Werd> = Vec::new();
    let mut cblobs: Vec<CBlob> = Vec::new();
    let mut prev_x = -i32::from(i16::MAX);
    let mut bol = true;
    let mut prev_blanks: u8 = 0;
    let mut prev_fuzzy_sp = false;
    let mut prev_fuzzy_non = false;
    let mut st = BreakState {
        blanks: 0,
        fuzzy_sp: false,
        fuzzy_non: false,
        prev_gap_was_a_space: false,
    };
    let max16 = i32::from(i16::MAX);
    let mut prev_gap;
    let mut current_gap = max16;
    let mut current_within_xht_gap = max16;
    let mut prev_blob_box;
    let mut box_it = Iter::new(&row.blobs);
    let first_left = blobs.get(box_it.data(&row.blobs)).bbox.left;
    if first_left > next_rep_char_word_right {
        let mut word = rep_words.pop_front().expect("rep word");
        word.set_flag(W_BOL, true);
        bol = false;
        word.set_blanks(0);
        word.set_flag(W_FUZZY_SP, false);
        word.set_flag(W_FUZZY_NON, false);
        let repetition_spacing = find_mean_blob_spacing(&word);
        current_gap = i32::from((first_left - next_rep_char_word_right) as i16);
        current_within_xht_gap = current_gap;
        if f64::from(current_gap) > TOSP_REP_SPACE * f64::from(repetition_spacing) {
            prev_blanks = (current_gap as f32 / row.space_size).floor() as u8;
            if prev_blanks < 1 {
                prev_blanks = 1;
            }
        } else {
            prev_blanks = 0;
        }
        words.push(word);
        next_rep_char_word_right = rep_words
            .front()
            .map_or(i32::MAX, |w| w.bounding_box().right);
    }
    let (mut next_blob_box, mut next_gap, mut next_within_xht_gap) =
        peek_at_next_gap(row, box_it, blobs);
    loop {
        let list = &row.blobs;
        let id = box_it.data(list);
        let b = blobs.get_mut(id);
        let blob_box = b.bbox;
        if b.joined {
            if let Some(cb) = b.cblob.take()
                && let Some(last) = cblobs.last_mut()
            {
                last.outlines.extend(cb.outlines);
            }
        } else {
            // The BLOBNBOX keeps its (no longer owned) blob, so later
            // tests still see one.
            if let Some(cb) = &b.cblob {
                cblobs.push(cb.clone());
            }
            prev_x = blob_box.right;
        }
        box_it.forward(list);
        let nid = box_it.data(list);
        let nb = blobs.get(nid);
        let blob_box = nb.bbox;
        if !nb.joined && nb.cblob.is_some() {
            prev_gap = current_gap;
            prev_blob_box = next_blob_box;
            current_gap = next_gap;
            current_within_xht_gap = next_within_xht_gap;
            (next_blob_box, next_gap, next_within_xht_gap) = peek_at_next_gap(row, box_it, blobs);
            let list = &row.blobs;
            let brk = blob_box.left > next_rep_char_word_right
                || make_a_word_break(
                    row,
                    &blob_box,
                    prev_gap,
                    &prev_blob_box,
                    current_gap,
                    current_within_xht_gap,
                    &next_blob_box,
                    next_gap,
                    &mut st,
                )
                || box_it.at_first(list);
            if brk {
                let mut word = Werd::new(std::mem::take(&mut cblobs), prev_blanks);
                if bol {
                    word.set_flag(W_BOL, true);
                    bol = false;
                }
                if prev_fuzzy_sp {
                    word.set_flag(W_FUZZY_SP, true);
                } else if prev_fuzzy_non {
                    word.set_flag(W_FUZZY_NON, true);
                }
                words.push(word);
                if blob_box.left > next_rep_char_word_right {
                    let mut word = rep_words.pop_front().expect("rep word");
                    let repetition_spacing = find_mean_blob_spacing(&word);
                    current_gap = i32::from((word.bounding_box().left - prev_x) as i16);
                    current_within_xht_gap = current_gap;
                    if f64::from(current_gap) > TOSP_REP_SPACE * f64::from(repetition_spacing) {
                        st.blanks = (current_gap as f32 / row.space_size).floor() as u8;
                        if st.blanks < 1 {
                            st.blanks = 1;
                        }
                    } else {
                        st.blanks = 0;
                    }
                    word.set_blanks(st.blanks);
                    word.set_flag(W_FUZZY_SP, false);
                    word.set_flag(W_FUZZY_NON, false);
                    current_gap = i32::from((blob_box.left - next_rep_char_word_right) as i16);
                    if f64::from(current_gap) > TOSP_REP_SPACE * f64::from(repetition_spacing) {
                        st.blanks = (current_gap as f32 / row.space_size) as u8;
                        if st.blanks < 1 {
                            st.blanks = 1;
                        }
                    } else {
                        st.blanks = 0;
                    }
                    st.fuzzy_sp = false;
                    st.fuzzy_non = false;
                    words.push(word);
                    next_rep_char_word_right = rep_words
                        .front()
                        .map_or(i32::MAX, |w| w.bounding_box().right);
                }
                let list = &row.blobs;
                if box_it.at_first(list) && rep_words.is_empty() {
                    words.last_mut().expect("word").set_flag(W_EOL, true);
                } else {
                    prev_blanks = st.blanks;
                    prev_fuzzy_sp = st.fuzzy_sp;
                    prev_fuzzy_non = st.fuzzy_non;
                }
            }
        }
        if box_it.at_first(&row.blobs) {
            break;
        }
    }
    while let Some(mut word) = rep_words.pop_front() {
        let repetition_spacing = find_mean_blob_spacing(&word);
        current_gap = i32::from((word.bounding_box().left - prev_x) as i16);
        let blanks = if f64::from(current_gap) > TOSP_REP_SPACE * f64::from(repetition_spacing) {
            ((current_gap as f32 / row.space_size).floor() as u8).max(1)
        } else {
            0
        };
        word.set_blanks(blanks);
        word.set_flag(W_FUZZY_SP, false);
        word.set_flag(W_FUZZY_NON, false);
        prev_x = word.bounding_box().right;
        if rep_words.is_empty() {
            word.set_flag(W_EOL, true);
        }
        words.push(word);
    }
    let _ = current_within_xht_gap;
    let mut real_row = PageRow::new(row, row.kern_size as i16, row.space_size as i16);
    real_row.words = words;
    real_row.recalc_bounding_box();
    Some(real_row)
}

impl Werd {
    pub fn set_blanks(&mut self, b: u8) {
        self.blanks = b;
    }
}

/// `fixed_pitch_words`.
fn fixed_pitch_words(row: &mut ToRow, blobs: &mut Blobs) -> Option<PageRow> {
    let mut rep_words: std::collections::VecDeque<Werd> = std::mem::take(&mut row.rep_words).into();
    let mut rep_left = rep_words
        .front()
        .map_or(i32::from(i16::MAX), |w| w.bounding_box().left);
    if row.blobs.is_empty() {
        return None;
    }
    if row.char_cells.len() < 2 {
        return None;
    }
    let mut arena = OutlineArena::default();
    let mut left: EList<u32> = EList::new();
    let mut right: EList<u32> = EList::new();
    let mut cblobs: Vec<CBlob> = Vec::new();
    let mut words: Vec<Werd> = Vec::new();
    let mut prev_x = -i32::from(i16::MAX);
    let mut bol = true;
    let mut blanks: u8 = 0;
    let pitch = row.fixed_pitch;
    let pitch_error = (f64::from(TEXTORD_FP_CHOP_ERROR) + 0.5) as f32;
    let cells = row.char_cells.clone();
    let mut prev_chop_coord = cells[0];
    let mut have_word = false;
    let mut last_was_rep = false;
    let add_repeated = |rep_words: &mut std::collections::VecDeque<Werd>,
                        rep_left: &mut i32,
                        prev_chop_coord: &mut i32,
                        blanks: &mut u8,
                        words: &mut Vec<Werd>| {
        if *rep_left > *prev_chop_coord {
            let nb =
                (f64::from(*rep_left - *prev_chop_coord) / f64::from(pitch) + 0.5).floor() as u8;
            *blanks = blanks.wrapping_add(nb);
        }
        let mut word = rep_words.pop_front().expect("rep word");
        *prev_chop_coord = i32::from(word.bounding_box().right as i16);
        word.set_blanks(*blanks);
        words.push(word);
        *rep_left = rep_words
            .front()
            .map_or(i32::from(i16::MAX), |w| w.bounding_box().left);
        *blanks = 0;
    };
    while rep_left < cells[0] {
        add_repeated(
            &mut rep_words,
            &mut rep_left,
            &mut prev_chop_coord,
            &mut blanks,
            &mut words,
        );
        have_word = true;
        last_was_rep = true;
    }
    let mut ci = 0usize;
    if prev_chop_coord >= cells[0] {
        ci = 1;
    }
    let list = &mut row.blobs;
    let mut box_it = Iter::new(list);
    while ci < cells.len() {
        let chop_coord = cells[ci];
        while !list.is_empty() && blobs.get(box_it.data(list)).bbox.left <= chop_coord {
            let id = box_it.data(list);
            if blobs.get(id).bbox.right > prev_x {
                prev_x = blobs.get(id).bbox.right;
            }
            let id = box_it.extract(list);
            let cb = blobs.get_mut(id).cblob.take();
            split_to_blob(
                cb,
                chop_coord as i16,
                pitch_error,
                &mut left,
                &mut right,
                &mut arena,
            );
            box_it.forward(list);
            while !list.is_empty() && !blobs.get(box_it.data(list)).cblob.is_some() {
                box_it.extract(list);
                box_it.forward(list);
            }
        }
        if !right.is_empty() && left.is_empty() {
            split_to_blob(
                None,
                chop_coord as i16,
                pitch_error,
                &mut left,
                &mut right,
                &mut arena,
            );
        }
        if !left.is_empty() {
            cblobs.push(arena.take_blob(&mut left));
        } else {
            let new_blanks: u8 = if rep_left < chop_coord {
                if rep_left > prev_chop_coord {
                    (f64::from(rep_left - prev_chop_coord) / f64::from(pitch) + 0.5).floor() as u8
                } else {
                    0
                }
            } else if chop_coord > prev_chop_coord {
                (f64::from(chop_coord - prev_chop_coord) / f64::from(pitch) + 0.5).floor() as u8
            } else {
                0
            };
            if !cblobs.is_empty() {
                if blanks < 1 && have_word && !last_was_rep {
                    blanks = 1;
                }
                let mut word = Werd::new(std::mem::take(&mut cblobs), blanks);
                word.set_flag(W_DONT_CHOP, true);
                if bol {
                    word.set_flag(W_BOL, true);
                    bol = false;
                }
                words.push(word);
                have_word = true;
                last_was_rep = false;
                blanks = new_blanks;
            } else {
                blanks = blanks.wrapping_add(new_blanks);
            }
            while rep_left < chop_coord {
                add_repeated(
                    &mut rep_words,
                    &mut rep_left,
                    &mut prev_chop_coord,
                    &mut blanks,
                    &mut words,
                );
                have_word = true;
                last_was_rep = true;
            }
        }
        if prev_chop_coord < chop_coord {
            prev_chop_coord = chop_coord;
        }
        ci += 1;
    }
    if !cblobs.is_empty() {
        let mut word = Werd::new(std::mem::take(&mut cblobs), blanks);
        word.set_flag(W_DONT_CHOP, true);
        if bol {
            word.set_flag(W_BOL, true);
        }
        words.push(word);
    }
    while !rep_words.is_empty() {
        add_repeated(
            &mut rep_words,
            &mut rep_left,
            &mut prev_chop_coord,
            &mut blanks,
            &mut words,
        );
    }
    if let Some(w) = words.last_mut() {
        w.set_flag(W_EOL, true);
    }
    let mut real_row = PageRow::new(row, row.kern_size as i16, row.space_size as i16);
    real_row.words = words;
    real_row.recalc_bounding_box();
    Some(real_row)
}
