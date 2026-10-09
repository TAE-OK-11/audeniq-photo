//! `Textord::TextordPage` for the sparse-text path: baseline detection
//! (`baselinedetect.cpp`), spline rows and x-heights (`makerow.cpp`,
//! `oldbasel.cpp`), underline separation (`underlin.cpp`, `blkocc.cpp`).
//! Word segmentation lives in [`super::wordseg`].

use super::blobbox::{BlobId, BlobNBox, Blobs, FlowType};
use super::colpartition::PolyBlockType;
use super::detlinefit::{DetLineFit, int_cast_rounded};
use super::elist::{EList, Iter};
use super::fit::{Llsq, QSpline, Qlsq};
use super::fpchop::{OutlineArena, split_to_blob};
use super::geom::{ICoord, TBox};
use super::layout::LayoutBlock;
use super::outline::{CBlob, find_cblob_limits};
use super::stats::Stats;
use super::stdalgo::nth_element;
use super::wordseg::Werd;
use std::f64::consts::PI;

// makerow.cpp / oldbasel.cpp / underlin.cpp parameters.
pub const TEXTORD_MIN_XHEIGHT: i32 = 10;
const TEXTORD_MINXH: f64 = 0.25;
const TEXTORD_SPLINE_MEDIANWIN: i32 = 6;
const TEXTORD_SPLINE_MINBLOBS: i32 = 8;
const TEXTORD_SPLINE_SHIFT_FRACTION: f64 = 0.02;
const TEXTORD_MIN_BLOB_HEIGHT_FRACTION: f64 = 0.75;
const TEXTORD_UNDERLINE_WIDTH: f64 = 2.0;
const TEXTORD_UNDERLINE_THRESHOLD: f64 = 0.5;
const TEXTORD_UNDERLINE_OFFSET: f64 = 0.1;
const TEXTORD_MAX_BLOB_OVERLAPS: i32 = 4;
const TEXTORD_CHOP_WIDTH: f64 = 1.5;
const TEXTORD_XHEIGHT_MODE_FRACTION: f64 = 0.4;
const TEXTORD_ASCHEIGHT_MODE_FRACTION: f64 = 0.08;
const TEXTORD_DESCHEIGHT_MODE_FRACTION: f64 = 0.08;
const TEXTORD_ASCX_RATIO_MIN: f64 = 1.25;
const TEXTORD_ASCX_RATIO_MAX: f64 = 1.8;
const TEXTORD_DESCX_RATIO_MIN: f64 = 0.25;
const TEXTORD_DESCX_RATIO_MAX: f64 = 0.6;
const TEXTORD_XHEIGHT_ERROR_MARGIN: f64 = 0.1;
pub const TEXTORD_FP_CHOP_ERROR: i32 = 2;
const OLDBL_JUMPLIMIT: f64 = 0.15;
const OLDBL_DOT_ERROR_SIZE: f64 = 1.26;
const OLDBL_HOLED_LOSSCOUNT: i32 = 10;
const MAX_HEIGHT_MODES: usize = 12;
const MIN_LEADER_COUNT: i32 = 5;
const TURNLIMIT: i32 = 1;
const MINASCRISE: f32 = 2.0;
const MAXHEIGHT: i32 = 300;
const MAXBADRUN: i32 = 2;
const MAXPARTS: usize = 6;
const SPLINESIZE: usize = 23;

const K_XHEIGHT_FRACTION: f64 = 0.5;
const K_ASCENDER_FRACTION: f64 = 0.25;
const K_DESCENDER_FRACTION: f64 = 0.25;
const K_XHEIGHT_CAP_RATIO: f64 = K_XHEIGHT_FRACTION / (K_XHEIGHT_FRACTION + K_ASCENDER_FRACTION);

// baselinedetect.cpp
const K_MAX_DISPLACEMENTS_MODES: usize = 3;
const K_NUM_SKIP_POINTS: usize = 3;
const K_MAX_SKEW_DEVIATION: f64 = 1.0 / 64.0;
const K_OFFSET_QUANTIZATION_FACTOR: f64 = 3.0 / 64.0;
const K_FIT_HALFRANGE_FACTOR: f64 = 6.0 / 64.0;
const K_MAX_BASELINE_ERROR: f64 = 3.0 / 64.0;
const K_MAX_BLOB_SIZE_MULTIPLE: f64 = 1.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitchType {
    Dunno,
    DefFixed,
    MaybeFixed,
    DefProp,
    MaybeProp,
    CorrFixed,
    CorrProp,
}

/// `TO_ROW`.
#[derive(Clone, Debug)]
pub struct ToRow {
    pub blobs: EList<BlobId>,
    pub y_min: f32,
    pub y_max: f32,
    pub initial_y_min: f32,
    pub m: f32,
    pub c: f32,
    pub error: f32,
    pub para_c: f32,
    pub para_error: f32,
    pub y_origin: f32,
    pub credibility: f32,
    pub num_repeated_sets: i32,
    pub all_caps: bool,
    pub used_dm_model: bool,
    pub projection_left: i32,
    pub projection_right: i32,
    pub pitch_decision: PitchType,
    pub fixed_pitch: f32,
    pub fp_space: f32,
    pub fp_nonsp: f32,
    pub pr_space: f32,
    pub pr_nonsp: f32,
    pub spacing: f32,
    pub xheight: f32,
    pub xheight_evidence: i32,
    pub ascrise: f32,
    pub descdrop: f32,
    pub body_size: f32,
    pub min_space: i32,
    pub max_nonspace: i32,
    pub space_threshold: i32,
    pub kern_size: f32,
    pub space_size: f32,
    pub rep_words: Vec<Werd>,
    /// `char_cells` (x coordinates; y is unused).
    pub char_cells: Vec<i32>,
    pub baseline: QSpline,
    pub projection: Stats,
}

impl ToRow {
    pub fn new(blobs: EList<BlobId>, y_min: f32, y_max: f32, initial_y_min: f32) -> ToRow {
        ToRow {
            blobs,
            y_min,
            y_max,
            initial_y_min,
            m: 0.0,
            c: 0.0,
            error: 0.0,
            para_c: 0.0,
            para_error: 0.0,
            y_origin: 0.0,
            credibility: 0.0,
            num_repeated_sets: -1,
            all_caps: false,
            used_dm_model: false,
            projection_left: 0,
            projection_right: 0,
            pitch_decision: PitchType::Dunno,
            fixed_pitch: 0.0,
            fp_space: 0.0,
            fp_nonsp: 0.0,
            pr_space: 0.0,
            pr_nonsp: 0.0,
            spacing: 0.0,
            xheight: 0.0,
            xheight_evidence: 0,
            ascrise: 0.0,
            descdrop: 0.0,
            body_size: 0.0,
            min_space: 0,
            max_nonspace: 0,
            space_threshold: 0,
            kern_size: 0.0,
            space_size: 0.0,
            rep_words: Vec::new(),
            char_cells: Vec::new(),
            baseline: QSpline::default(),
            projection: Stats::default(),
        }
    }

    pub fn rep_chars_marked(&self) -> bool {
        self.num_repeated_sets != -1
    }

    fn set_line(&mut self, m: f32, c: f32, error: f32) {
        self.m = m;
        self.c = c;
        self.error = error;
    }

    fn set_parallel_line(&mut self, gradient: f32, new_c: f32, new_error: f32) {
        self.para_c = new_c;
        self.para_error = new_error;
        self.credibility = self.blobs.len() as f32 - 3.0 * new_error;
        self.y_origin = new_c / (1.0 + gradient * gradient).sqrt();
    }

    /// `TO_ROW::insert_blob`.
    pub fn insert_blob(&mut self, blobs: &Blobs, blob: BlobId) {
        let list = &mut self.blobs;
        let mut it = Iter::new(list);
        if list.is_empty() {
            it.add_before_then_move(list, blob);
        } else {
            let left = blobs.get(blob).bbox.left;
            it.mark_cycle_pt();
            while !it.cycled_list(list) && blobs.get(it.data(list)).bbox.left <= left {
                it.forward(list);
            }
            if it.cycled_list(list) {
                it.add_to_end(list, blob);
            } else {
                it.add_before_stay_put(list, blob);
            }
        }
    }

    /// `TO_ROW::compute_vertical_projection`.
    pub fn compute_vertical_projection(&mut self, blobs: &Blobs) {
        let ids = self.blobs.to_vec();
        if ids.is_empty() {
            return;
        }
        let mut row_box = blobs.get(ids[0]).bbox;
        for &id in &ids {
            row_box.union_with(&blobs.get(id).bbox);
        }
        const PROJECTION_MARGIN: i32 = 10;
        self.projection = Stats::new(
            row_box.left - PROJECTION_MARGIN,
            row_box.right + PROJECTION_MARGIN - 1,
        );
        self.projection_left = i32::from((row_box.left - PROJECTION_MARGIN) as i16);
        self.projection_right = i32::from((row_box.right + PROJECTION_MARGIN) as i16);
        for &id in &ids {
            if let Some(cb) = &blobs.get(id).cblob {
                for o in &cb.outlines {
                    vertical_coutline_projection(o, &mut self.projection);
                }
            }
        }
    }
}

fn vertical_coutline_projection(o: &super::outline::Outline, stats: &mut Stats) {
    let mut pos = o.start;
    for s in 0..o.steps.len() {
        let st = o.step(s);
        if st.x > 0 {
            stats.add(pos.x, -pos.y);
        } else if st.x < 0 {
            stats.add(pos.x - 1, pos.y);
        }
        pos += st;
    }
    for c in &o.children {
        vertical_coutline_projection(c, stats);
    }
}

/// `TO_BLOCK` with its `BLOCK`.
#[derive(Clone, Debug)]
pub struct ToBlk {
    pub bbox: TBox,
    pub ptype: PolyBlockType,
    pub line_size: f32,
    pub line_spacing: f32,
    pub max_blob_size: f32,
    pub baseline_offset: f32,
    pub xheight: f32,
    pub pitch_decision: PitchType,
    pub fixed_pitch: f32,
    pub kern_size: f32,
    pub space_size: f32,
    pub min_space: i32,
    pub max_nonspace: i32,
    pub fp_space: f32,
    pub fp_nonsp: f32,
    pub pr_space: f32,
    pub pr_nonsp: f32,
    pub rows: Vec<ToRow>,
    pub underlines: EList<BlobId>,
    pub large_blobs: EList<BlobId>,
    /// `BLOCK::xheight` (an integer).
    pub block_xheight: i32,
}

impl ToBlk {
    pub fn from_layout(b: LayoutBlock) -> ToBlk {
        let row = ToRow::new(b.row.blobs, b.row.y_min, b.row.y_max, b.row.initial_y_min);
        ToBlk {
            bbox: b.bbox,
            ptype: b.ptype,
            line_size: b.line_size,
            line_spacing: b.line_spacing,
            max_blob_size: b.max_blob_size,
            baseline_offset: 0.0,
            xheight: 0.0,
            pitch_decision: PitchType::Dunno,
            fixed_pitch: 0.0,
            kern_size: 0.0,
            space_size: 0.0,
            min_space: 0,
            max_nonspace: 0,
            fp_space: 0.0,
            fp_nonsp: 0.0,
            pr_space: 0.0,
            pr_nonsp: 0.0,
            rows: vec![row],
            underlines: EList::new(),
            large_blobs: EList::new(),
            block_xheight: 0,
        }
    }

    pub fn is_text(&self) -> bool {
        self.ptype.is_text()
    }
}

fn fangle(x: f32, y: f32) -> f64 {
    f64::from(y.atan2(x))
}

/// `BaselineRow`.
struct BaselineRow {
    fitter: DetLineFit,
    pt1: (f32, f32),
    pt2: (f32, f32),
    disp_quant_factor: f64,
    fit_halfrange: f64,
    max_baseline_error: f64,
    baseline_error: f64,
    good_baseline: bool,
    displacement_modes: Vec<f64>,
}

impl BaselineRow {
    fn new(line_spacing: f64) -> BaselineRow {
        BaselineRow {
            fitter: DetLineFit::new(),
            pt1: (0.0, 0.0),
            pt2: (0.0, 0.0),
            disp_quant_factor: K_OFFSET_QUANTIZATION_FACTOR * line_spacing,
            fit_halfrange: K_FIT_HALFRANGE_FACTOR * line_spacing,
            max_baseline_error: K_MAX_BASELINE_ERROR * line_spacing,
            baseline_error: 0.0,
            good_baseline: false,
            displacement_modes: Vec::new(),
        }
    }

    fn baseline_angle(&self) -> f64 {
        let angle = fangle(self.pt2.0 - self.pt1.0, self.pt2.1 - self.pt1.1);
        (angle + PI * 1.5) % PI - PI * 0.5
    }

    fn straight_y_at_x(&self, x: f64) -> f64 {
        let denominator = f64::from(self.pt2.0 - self.pt1.0);
        if denominator == 0.0 {
            return (f64::from(self.pt1.1) + f64::from(self.pt2.1)) / 2.0;
        }
        f64::from(self.pt1.1)
            + (x - f64::from(self.pt1.0)) * f64::from(self.pt2.1 - self.pt1.1) / denominator
    }

    fn setup_old_line_parameters(&self, row: &mut ToRow) {
        let gradient = self.baseline_angle().tan() as f32;
        let para_c = self.straight_y_at_x(0.0) as f32;
        row.set_line(gradient, para_c, self.baseline_error as f32);
        row.set_parallel_line(gradient, para_c, self.baseline_error as f32);
    }

    fn fit_baseline(&mut self, row: &ToRow, blobs: &mut Blobs, use_box_bottoms: bool) -> bool {
        self.fitter.clear();
        let mut llsq = Llsq::default();
        for id in row.blobs.to_vec() {
            let b = blobs.get_mut(id);
            if !use_box_bottoms {
                b.baseline_y = match &b.cblob {
                    Some(cb) => cb.estimate_baseline_position(),
                    None => b.bbox.bottom,
                };
            }
            let x_middle = (b.bbox.left + b.bbox.right) / 2;
            self.fitter
                .add_width(ICoord::new(x_middle, b.baseline_y), b.bbox.width() / 2);
            llsq.add(f64::from(x_middle), f64::from(b.baseline_y));
        }
        let (err, mut pt1, mut pt2) = self.fitter.fit();
        self.baseline_error = err;
        self.pt1 = (pt1.x as f32, pt1.y as f32);
        self.pt2 = (pt2.x as f32, pt2.y as f32);
        if self.baseline_error > self.max_baseline_error
            && self.fitter.sufficient_points_for_independent_fit()
        {
            let (error, p1, p2) = self.fitter.fit_skip(K_NUM_SKIP_POINTS, K_NUM_SKIP_POINTS);
            pt1 = p1;
            pt2 = p2;
            if error < self.baseline_error / 2.0 {
                self.baseline_error = error;
                self.pt1 = (pt1.x as f32, pt1.y as f32);
                self.pt2 = (pt2.x as f32, pt2.y as f32);
            }
        }
        let direction = ((pt2.x - pt1.x) as f32, (pt2.y - pt1.y) as f32);
        let target_offset = f64::from(direction.0 * pt1.y as f32 - direction.1 * pt1.x as f32);
        self.good_baseline = false;
        self.fit_constrained_if_better(direction, 0.0, target_offset);
        let angle = self.baseline_angle();
        if angle.abs() > PI * 0.25 {
            self.pt1 = llsq.mean_point();
            self.pt2 = (self.pt1.0 + 1.0, self.pt1.1 + llsq.m() as f32);
            let m = llsq.m();
            let c = llsq.c(m);
            self.baseline_error = llsq.rms(m, c);
            self.good_baseline = false;
        }
        self.good_baseline
    }

    fn adjust_baseline_to_parallel(&mut self, row: &ToRow, blobs: &Blobs, direction: (f32, f32)) {
        self.setup_blob_displacements(row, blobs, direction);
        if self.displacement_modes.is_empty() {
            return;
        }
        let target = self.displacement_modes[0];
        self.fit_constrained_if_better(direction, 0.0, target);
    }

    fn setup_blob_displacements(&mut self, row: &ToRow, blobs: &Blobs, direction: (f32, f32)) {
        let mut perp = Vec::new();
        self.displacement_modes.clear();
        let mut min_dist = f64::from(f32::MAX);
        let mut max_dist = -f64::from(f32::MAX);
        for id in row.blobs.to_vec() {
            let b = blobs.get(id);
            let px = (b.bbox.left + b.bbox.right) as f32 / 2.0;
            let py = b.baseline_y as f32;
            let offset = f64::from(direction.0 * py - direction.1 * px);
            perp.push(offset);
            if offset < min_dist {
                min_dist = offset;
            }
            if offset > max_dist {
                max_dist = offset;
            }
        }
        let q = self.disp_quant_factor;
        let mut stats = Stats::new(
            int_cast_rounded(min_dist / q),
            int_cast_rounded(max_dist / q),
        );
        for d in perp {
            stats.add(int_cast_rounded(d / q), 1);
        }
        for (key, _) in stats.top_n_modes(K_MAX_DISPLACEMENTS_MODES) {
            self.displacement_modes.push(q * f64::from(key));
        }
    }

    fn fit_constrained_if_better(
        &mut self,
        direction: (f32, f32),
        cheat_allowance: f64,
        target_offset: f64,
    ) {
        let length = (direction.0 * direction.0 + direction.1 * direction.1).sqrt();
        let halfrange = self.fit_halfrange * f64::from(length);
        let (mut new_error, line_pt) = self.fitter.constrained_fit_dir(
            direction,
            target_offset - halfrange,
            target_offset + halfrange,
        );
        new_error -= cheat_allowance;
        let old_angle = self.baseline_angle();
        let new_angle = fangle(direction.0, direction.1);
        let new_good = new_error <= self.max_baseline_error
            && (cheat_allowance > 0.0 || self.fitter.sufficient_points_for_independent_fit());
        if new_error <= self.baseline_error
            || (!self.good_baseline && new_good)
            || (new_angle - old_angle).abs() > K_MAX_SKEW_DEVIATION
        {
            self.baseline_error = new_error;
            self.pt1 = (line_pt.x as f32, line_pt.y as f32);
            self.pt2 = (self.pt1.0 + direction.0, self.pt1.1 + direction.1);
            self.good_baseline = new_good;
        }
    }
}

/// `BaselineDetect` + `ComputeStraightBaselines` +
/// `ComputeBaselineSplinesAndXheights` over all blocks.
pub fn baseline_detect(blocks: &mut [ToBlk], blobs: &mut Blobs) {
    let use_box_bottoms = false;
    // BaselineBlock constructors.
    let mut rows: Vec<Vec<BaselineRow>> = Vec::with_capacity(blocks.len());
    for block in blocks.iter_mut() {
        let mut brows = Vec::new();
        for row in &mut block.rows {
            row.blobs
                .sort_by(|&a, &b| blobs.get(a).bbox.left.cmp(&blobs.get(b).bbox.left));
            brows.push(BaselineRow::new(f64::from(block.line_spacing)));
        }
        rows.push(brows);
    }
    let mut skew = vec![0.0f64; blocks.len()];
    let mut good_skew = vec![false; blocks.len()];
    let mut block_skew_angles = Vec::new();
    for (bi, block) in blocks.iter().enumerate() {
        if !block.is_text() {
            continue;
        }
        let mut angles = Vec::new();
        for (ri, brow) in rows[bi].iter_mut().enumerate() {
            if brow.fit_baseline(&block.rows[ri], blobs, use_box_bottoms) {
                angles.push(brow.baseline_angle());
            }
        }
        if !angles.is_empty() {
            skew[bi] = median_of_circular_values(&mut angles);
            good_skew[bi] = true;
            block_skew_angles.push(skew[bi]);
        }
    }
    let mut default_block_skew = 0.0f64;
    if !block_skew_angles.is_empty() {
        default_block_skew = median_of_circular_values(&mut block_skew_angles);
    }
    for (bi, block) in blocks.iter_mut().enumerate() {
        // ParallelizeBaselines.
        if block.is_text() {
            if !good_skew[bi] {
                skew[bi] = default_block_skew;
            }
            let direction = (skew[bi].cos() as f32, skew[bi].sin() as f32);
            for (ri, brow) in rows[bi].iter_mut().enumerate() {
                brow.adjust_baseline_to_parallel(&block.rows[ri], blobs, direction);
            }
            // rows < 3: no line spacing model.
            debug_assert!(rows[bi].len() < 3, "multi-row blocks are not ported");
        }
        // SetupBlockParameters.
        let line_spacing = f64::from(block.line_spacing);
        if line_spacing > 0.0 {
            let min_spacing = block.line_spacing.min(line_spacing as f32);
            if min_spacing < block.line_size {
                block.line_size = min_spacing;
            }
            block.line_spacing = line_spacing as f32;
            block.baseline_offset = 0.0;
            block.max_blob_size = (line_spacing * K_MAX_BLOB_SIZE_MULTIPLE) as f32;
        }
        for (ri, brow) in rows[bi].iter().enumerate() {
            brow.setup_old_line_parameters(&mut block.rows[ri]);
        }
    }
    for (bi, block) in blocks.iter_mut().enumerate() {
        if !block.is_text() {
            // FitBaselineSplines still runs for non-text blocks.
            fit_baseline_splines(block, blobs, skew[bi]);
            continue;
        }
        // PrepareForSplineFitting.
        let gradient = skew[bi].tan() as f32;
        separate_underlines(block, blobs, gradient);
        pre_associate_blobs(block, blobs);
        fit_baseline_splines(block, blobs, skew[bi]);
    }
}

/// `MedianOfCircularValues` as compiled: the offset loops modify copies, so
/// this is the plain median.
fn median_of_circular_values(v: &mut [f64]) -> f64 {
    let median_index = v.len() / 2;
    nth_element(v, median_index, |a, b| a < b);
    v[median_index]
}

fn fit_baseline_splines(block: &mut ToBlk, blobs: &mut Blobs, skew: f64) {
    let gradient = skew.tan() as f32;
    make_spline_rows(block, blobs, gradient);
    compute_block_xheight(block, blobs, gradient);
    block.block_xheight = block.xheight as i32;
    restore_underlined_blobs(block, blobs);
}

/// `separate_underlines`.
fn separate_underlines(block: &mut ToBlk, blobs: &mut Blobs, gradient: f32) {
    let min_blob_height =
        (TEXTORD_MIN_BLOB_HEIGHT_FRACTION * f64::from(block.line_size) + 0.5) as i32;
    let length = (1.0 + gradient * gradient).sqrt();
    let g_vec = (1.0 / length, -gradient / length);
    // blob_rotation = (1, -0) rotated by g_vec.
    let (rx, ry) = (1.0f32, -0.0f32);
    let blob_rotation = (rx * g_vec.0 - ry * g_vec.1, ry * g_vec.0 + rx * g_vec.1);
    let xheight_arg =
        (f64::from(block.line_size) * (K_XHEIGHT_FRACTION + K_ASCENDER_FRACTION / 2.0)) as i16;
    let line_size = block.line_size;
    for ri in 0..block.rows.len() {
        let intercept = block.rows[ri].y_origin as i16;
        let list = &mut block.rows[ri].blobs;
        let mut it = Iter::new(list);
        it.mark_cycle_pt();
        while !it.cycled_list(list) {
            let id = it.data(list);
            let bbox = blobs.get(id).bbox;
            if f64::from(bbox.width()) > f64::from(line_size) * TEXTORD_UNDERLINE_WIDTH {
                let cb = blobs
                    .get(id)
                    .cblob
                    .as_ref()
                    .expect("underline candidate has cblob");
                let rotated: Vec<_> = cb
                    .outlines
                    .iter()
                    .map(|o| o.rotated(blob_rotation))
                    .collect();
                if test_underline(&rotated, i32::from(intercept), i32::from(xheight_arg)) {
                    let b = it.extract(list);
                    let mut uit = Iter::new(&block.underlines);
                    uit.move_to_last(&block.underlines);
                    uit.add_after_then_move(&mut block.underlines, b);
                } else if count_overlaps(&bbox, min_blob_height, list, blobs)
                    > TEXTORD_MAX_BLOB_OVERLAPS
                {
                    let b = it.extract(list);
                    let mut lit = Iter::new(&block.large_blobs);
                    lit.move_to_last(&block.large_blobs);
                    lit.add_after_then_move(&mut block.large_blobs, b);
                }
            }
            it.forward(list);
        }
    }
}

fn count_overlaps(bbox: &TBox, min_height: i32, list: &EList<BlobId>, blobs: &Blobs) -> i32 {
    list.to_vec()
        .into_iter()
        .filter(|&id| {
            let b = blobs.get(id).bbox;
            b.height() >= min_height && bbox.major_overlap(&b)
        })
        .count() as i32
}

/// `test_underline` on the rotated top-level outlines of a blob.
fn test_underline(outlines: &[super::outline::Outline], baseline: i32, xheight: i32) -> bool {
    let mut blob_box = TBox::default();
    for o in outlines {
        blob_box.union_with(&o.bbox);
    }
    let blob_width = blob_box.width();
    let mut projection = Stats::new(blob_box.bottom, blob_box.top);
    for o in outlines {
        horizontal_coutline_projection(o, &mut projection);
    }
    let mut desc_occ = 0;
    let mut occ = blob_box.bottom;
    while occ < baseline {
        if occ <= blob_box.top && projection.pile_count(occ) > desc_occ {
            desc_occ = projection.pile_count(occ);
        }
        occ += 1;
    }
    let mut x_occ = 0;
    for occ in baseline..=baseline + xheight {
        if occ >= blob_box.bottom && occ <= blob_box.top && projection.pile_count(occ) > x_occ {
            x_occ = projection.pile_count(occ);
        }
    }
    let mut asc_occ = 0;
    for occ in baseline + xheight + 1..=blob_box.top {
        if occ >= blob_box.bottom && projection.pile_count(occ) > asc_occ {
            asc_occ = projection.pile_count(occ);
        }
    }
    let thresh = f64::from(blob_width) * TEXTORD_UNDERLINE_THRESHOLD;
    if desc_occ > x_occ + x_occ && f64::from(desc_occ) > thresh {
        return true;
    }
    asc_occ > x_occ + x_occ && f64::from(asc_occ) > thresh
}

fn horizontal_coutline_projection(o: &super::outline::Outline, stats: &mut Stats) {
    let mut pos = o.start;
    for s in 0..o.steps.len() {
        let st = o.step(s);
        if st.y > 0 {
            stats.add(pos.y, pos.x);
        } else if st.y < 0 {
            stats.add(pos.y - 1, -pos.x);
        }
        pos += st;
    }
    for c in &o.children {
        horizontal_coutline_projection(c, stats);
    }
}

/// `pre_associate_blobs`: merge x-overlapping blobs and chop wide ones.
fn pre_associate_blobs(block: &mut ToBlk, blobs: &mut Blobs) {
    let chop_xheight =
        (f64::from(block.line_size) * K_XHEIGHT_FRACTION * TEXTORD_CHOP_WIDTH) as f32;
    for row in &mut block.rows {
        let list = &mut row.blobs;
        let mut it = Iter::new(list);
        it.mark_cycle_pt();
        while !it.cycled_list(list) {
            let id = it.data(list);
            let mut blob_box = blobs.get(id).bbox;
            let start_it = it;
            loop {
                let mut overlap = false;
                if !it.at_last(list) {
                    let next = it.data_relative(list, 1);
                    overlap = blob_box.major_x_overlap(&blobs.get(next).bbox);
                    if overlap {
                        let nb = blobs.get(next).bbox;
                        let b = blobs.get_mut(id);
                        b.bbox.union_with(&nb);
                        let bb = b.bbox;
                        b.set_diacritic_box(&bb);
                        blobs.get_mut(next).joined = true;
                        blob_box = blobs.get(id).bbox;
                        it.forward(list);
                    }
                }
                if !overlap {
                    break;
                }
            }
            chop_blob(id, start_it, &mut it, list, blobs, chop_xheight);
            it.forward(list);
        }
    }
}

/// `BLOBNBOX::chop`.
fn chop_blob(
    id: BlobId,
    start_it: Iter,
    end_it: &mut Iter,
    list: &mut EList<BlobId>,
    blobs: &mut Blobs,
    xheight: f32,
) {
    let bbox = blobs.get(id).bbox;
    let blobcount = (bbox.width() as f32 / xheight).floor() as i16;
    if blobcount <= 1 || blobs.get(id).cblob.is_none() {
        return;
    }
    let blobwidth = (bbox.width() + 1) as f32 / f32::from(blobcount);
    let mut rightx = bbox.right as f32;
    let end_data = end_it.data(list);
    let mut blobindex = blobcount - 1;
    while blobindex >= 0 {
        let mut ymin = i32::MAX as f32;
        let mut ymax = -i32::MAX as f32;
        let mut bit = start_it;
        loop {
            let b = bit.data(list);
            let (lo, hi) = match &blobs.get(b).cblob {
                Some(cb) => find_cblob_limits(cb, rightx - blobwidth, rightx),
                None => (i32::MAX as f32, -i32::MAX as f32),
            };
            bit.forward(list);
            if lo < ymin {
                ymin = lo;
            }
            if hi > ymax {
                ymax = hi;
            }
            if b == end_data {
                break;
            }
        }
        if ymin < ymax {
            let mut leftx = (rightx - blobwidth).floor() as i16;
            if i32::from(leftx) < bbox.left {
                leftx = bbox.left as i16;
            }
            let bl = ICoord::new(i32::from(leftx), i32::from(ymin.floor() as i16));
            let tr = ICoord::new(
                i32::from(rightx.ceil() as i16),
                i32::from(ymax.ceil() as i16),
            );
            if blobindex == 0 {
                blobs.get_mut(id).bbox = TBox::from_corners(bl, tr);
            } else {
                let nb = blobs.add(BlobNBox::fake(TBox::from_corners(bl, tr)));
                end_it.add_after_stay_put(list, nb);
            }
        }
        blobindex -= 1;
        rightx -= blobwidth;
    }
}

/// `box_next_pre_chopped`.
pub fn box_next_pre_chopped(it: &mut Iter, list: &EList<BlobId>, blobs: &Blobs) -> TBox {
    let result = blobs.get(it.data(list)).bbox;
    loop {
        it.forward(list);
        if !blobs.get(it.data(list)).joined {
            break;
        }
    }
    result
}

/// `box_next`.
pub fn box_next(it: &mut Iter, list: &EList<BlobId>, blobs: &Blobs) -> TBox {
    let mut result = blobs.get(it.data(list)).bbox;
    loop {
        it.forward(list);
        let b = blobs.get(it.data(list));
        if b.cblob.is_none() {
            result.union_with(&b.bbox);
        }
        if !(b.cblob.is_none() || b.joined) {
            break;
        }
    }
    result
}

/// `make_spline_rows`.
fn make_spline_rows(block: &mut ToBlk, blobs: &mut Blobs, gradient: f32) {
    block.rows.retain(|r| !r.blobs.is_empty());
    let line_size = block.line_size;
    for row in &mut block.rows {
        make_baseline_spline(row, line_size, blobs);
    }
    make_old_baselines(block, blobs, gradient);
}

fn make_baseline_spline(row: &mut ToRow, line_size: f32, blobs: &Blobs) {
    let mut xstarts = vec![0i32; row.blobs.len() + 1];
    let segments = segment_baseline(row, line_size, &mut xstarts, blobs);
    // textord_parallel_baselines: always the straight line.
    xstarts[1] = xstarts[segments];
    let coeffs = [0.0, f64::from(row.m), f64::from(row.c)];
    row.baseline = QSpline::from_coeffs(&xstarts[..2], &coeffs);
}

/// `segment_baseline`: only the segment ends are used (the straight
/// baseline is always chosen); returns the segment count.
fn segment_baseline(row: &ToRow, line_size: f32, xstarts: &mut [i32], blobs: &Blobs) -> usize {
    let list = &row.blobs;
    let mut blob_it = Iter::new(list);
    let mut new_it = blob_it;
    let mut sorted: Vec<(f32, i32)> = Vec::new();
    let mut bx = box_next_pre_chopped(&mut blob_it, list, blobs);
    xstarts[0] = bx.left;
    let mut segments = 1usize;
    let mut blobcount = list.len() as i32;
    if blobcount <= TEXTORD_SPLINE_MEDIANWIN || blobcount < TEXTORD_SPLINE_MINBLOBS {
        blob_it.move_to_last(list);
        xstarts[1] = blobs.get(blob_it.data(list)).bbox.right;
        return 1;
    }
    let mut last_state = 0;
    new_it.mark_cycle_pt();
    let mut blobindex = 0;
    let add = |v: &mut Vec<(f32, i32)>, value: f32, key: i32| {
        let pos = v.iter().position(|e| e.0 >= value).unwrap_or(v.len());
        v.insert(pos, (value, key));
    };
    while blobindex < TEXTORD_SPLINE_MEDIANWIN {
        let nb = box_next_pre_chopped(&mut new_it, list, blobs);
        let middle = (f64::from(nb.left + nb.right) / 2.0) as f32;
        let yshift = (nb.bottom as f32 - row.m * middle) - row.c;
        add(&mut sorted, yshift, blobindex);
        if new_it.cycled_list(list) {
            xstarts[1] = nb.right;
            return 1;
        }
        blobindex += 1;
    }
    blobcount = 0;
    while blobcount < TEXTORD_SPLINE_MEDIANWIN / 2 {
        bx = box_next_pre_chopped(&mut blob_it, list, blobs);
        blobcount += 1;
    }
    let mut new_box;
    loop {
        new_box = box_next_pre_chopped(&mut new_it, list, blobs);
        let yshift = sorted[(TEXTORD_SPLINE_MEDIANWIN / 2) as usize].0;
        let lim = TEXTORD_SPLINE_SHIFT_FRACTION * f64::from(line_size);
        let state = if f64::from(yshift) > lim {
            1
        } else if f64::from(-yshift) > lim {
            -1
        } else {
            0
        };
        if state != last_state && blobcount > TEXTORD_SPLINE_MINBLOBS {
            xstarts[segments] = bx.left;
            segments += 1;
            blobcount = 0;
        }
        last_state = state;
        if let Some(p) = sorted
            .iter()
            .position(|e| e.1 == blobindex - TEXTORD_SPLINE_MEDIANWIN)
        {
            sorted.remove(p);
        }
        bx = box_next_pre_chopped(&mut blob_it, list, blobs);
        let middle = (f64::from(new_box.left + new_box.right) / 2.0) as f32;
        let yshift = (new_box.bottom as f32 - row.m * middle) - row.c;
        add(&mut sorted, yshift, blobindex);
        blobindex += 1;
        blobcount += 1;
        if new_it.cycled_list(list) {
            break;
        }
    }
    if blobcount > TEXTORD_SPLINE_MINBLOBS || segments == 1 {
        xstarts[segments] = new_box.right;
    } else {
        segments -= 1;
        xstarts[segments] = new_box.right;
    }
    segments
}

/// `make_old_baselines`.
fn make_old_baselines(block: &mut ToBlk, blobs: &mut Blobs, gradient: f32) {
    let line_size = block.line_size;
    let bbox = block.bbox;
    for ri in 0..block.rows.len() {
        find_textlines(&mut block.rows[ri], line_size, bbox, blobs);
        // A failed row would retry from the previous row's baseline; with
        // one row per block there is none.
    }
    // correlate_lines.
    if block.rows.is_empty() {
        block.xheight = block.line_size;
    } else {
        for row in &mut block.rows {
            if row.xheight < 0.0 {
                row.xheight = -row.xheight;
            }
        }
        compute_block_xheight(block, blobs, gradient);
    }
    block.block_xheight = block.xheight as i32;
}

/// `find_textlines` with no starting spline.
fn find_textlines(row: &mut ToRow, line_size: f32, block_box: TBox, blobs: &mut Blobs) {
    let blobcount = row.blobs.len();
    let mut partids = vec![0i8; blobcount];
    let mut xcoords = vec![0i32; blobcount];
    let mut ycoords = vec![0i32; blobcount];
    let mut blobcoords = vec![TBox::default(); blobcount];
    let mut ydiffs = vec![0f32; blobcount];
    let (lineheight, holed_line, blobcount) =
        get_blob_coords(row, line_size as i32, &mut blobcoords, blobs);
    let mut jumplimit = (f64::from(lineheight) * OLDBL_JUMPLIMIT) as f32;
    if jumplimit < MINASCRISE {
        jumplimit = MINASCRISE;
    }
    if holed_line {
        let line_m = row.m;
        make_holed_baseline(&blobcoords[..blobcount], &mut row.baseline, line_m);
    }
    // make_first_baseline: textord_oldbl_paradef keeps the default.
    if blobcount > 1 {
        let mut partsizes = [0i32; MAXPARTS];
        let (bestpart, partcount) = partition_line(
            &blobcoords[..blobcount],
            &mut partids,
            &mut partsizes,
            &row.baseline,
            jumplimit,
            &mut ydiffs,
        );
        let pointcount = partition_coords(
            &blobcoords[..blobcount],
            &partids,
            bestpart,
            &mut xcoords,
            &mut ycoords,
        );
        let mut xstarts = [0i32; SPLINESIZE + 1];
        let mut segments = segment_spline(&xcoords, &ycoords, 2, pointcount, &mut xstarts);
        if !holed_line {
            loop {
                row.baseline = QSpline::fit(&xstarts, segments, &xcoords, &ycoords, pointcount, 2);
                if !split_stepped_spline(
                    &row.baseline,
                    jumplimit / 2.0,
                    &xcoords,
                    &mut xstarts,
                    &mut segments,
                ) {
                    break;
                }
            }
        }
        find_lesser_parts(
            row,
            &blobcoords[..blobcount],
            &partids,
            &partsizes,
            partcount,
            bestpart,
        );
    } else {
        row.xheight = -1.0;
        row.descdrop = 0.0;
        row.ascrise = 0.0;
    }
    let m = f64::from(row.m);
    row.baseline.extrapolate(m, block_box.left, block_box.right);
    let line_m = row.m;
    compute_row_xheight(row, line_m, line_size as i32, blobs);
}

// Branches mirror the C++ conditions one for one.
#[allow(clippy::if_same_then_else)]
/// `get_blob_coords`: returns (x-height guess, holed, blob count).
fn get_blob_coords(
    row: &ToRow,
    lineheight: i32,
    blobcoords: &mut [TBox],
    blobs: &Blobs,
) -> (i32, bool, usize) {
    let list = &row.blobs;
    if list.is_empty() {
        return (0, false, 0);
    }
    let mut heightstat = Stats::new(0, MAXHEIGHT - 1);
    let mut maxlosscount = 0;
    let mut losscount = 0;
    let mut it = Iter::new(list);
    it.mark_cycle_pt();
    let mut blobindex = 0usize;
    let quarter = f64::from(lineheight) * 0.25;
    loop {
        blobcoords[blobindex] = box_next_pre_chopped(&mut it, list, blobs);
        let b = blobcoords[blobindex];
        if f64::from(b.height()) > quarter {
            heightstat.add(b.height(), 1);
        }
        if blobindex == 0 || f64::from(b.height()) > quarter || it.cycled_list(list) {
            blobindex += 1;
            losscount = 0;
        } else if f64::from(b.height()) < f64::from(b.width()) * OLDBL_DOT_ERROR_SIZE
            && f64::from(b.width()) < f64::from(b.height()) * OLDBL_DOT_ERROR_SIZE
        {
            blobindex += 1;
            losscount = 0;
        } else {
            losscount += 1;
            if losscount > maxlosscount {
                maxlosscount = losscount;
            }
        }
        if it.cycled_list(list) {
            break;
        }
    }
    let holed = maxlosscount > OLDBL_HOLED_LOSSCOUNT;
    let lh = if heightstat.get_total() > 1 {
        heightstat.ile(0.25) as i32
    } else {
        blobcoords[0].height()
    };
    (lh, holed, blobindex)
}

/// `make_holed_baseline` with no starting spline.
fn make_holed_baseline(blobcoords: &[TBox], baseline: &mut QSpline, gradient: f32) {
    let mut lms = DetLineFit::new();
    for b in blobcoords {
        lms.add(ICoord::new((b.left + b.right) / 2, b.bottom));
    }
    let (_, c) = lms.constrained_fit_m(f64::from(gradient));
    let xstarts = [blobcoords[0].left, blobcoords[blobcoords.len() - 1].right];
    let coeffs = [0.0, f64::from(gradient), f64::from(c)];
    *baseline = QSpline::from_coeffs(&xstarts, &coeffs);
}

/// `partition_line`: returns (biggest partition, partition count).
fn partition_line(
    blobcoords: &[TBox],
    partids: &mut [i8],
    partsizes: &mut [i32; MAXPARTS],
    spline: &QSpline,
    jumplimit: f32,
    ydiffs: &mut [f32],
) -> (i32, usize) {
    let blobcount = blobcoords.len();
    let mut partdiffs = [0f32; MAXPARTS];
    *partsizes = [0; MAXPARTS];
    let startx = get_ydiffs(blobcoords, spline, ydiffs);
    let mut numparts = 1usize;
    let mut bestpart: i32 = -1;
    let mut drift = 0.0f32;
    let mut last_delta = 0.0f32;
    for blobindex in startx..blobcount {
        bestpart = choose_partition(
            ydiffs[blobindex],
            &mut partdiffs,
            bestpart,
            jumplimit,
            &mut drift,
            &mut last_delta,
            &mut numparts,
        );
        partids[blobindex] = bestpart as i8;
        partsizes[bestpart as usize] += 1;
    }
    bestpart = -1;
    drift = 0.0;
    last_delta = 0.0;
    partsizes[0] -= 1;
    for blobindex in (0..=startx).rev() {
        bestpart = choose_partition(
            ydiffs[blobindex],
            &mut partdiffs,
            bestpart,
            jumplimit,
            &mut drift,
            &mut last_delta,
            &mut numparts,
        );
        partids[blobindex] = bestpart as i8;
        partsizes[bestpart as usize] += 1;
    }
    let mut biggestpart = 0usize;
    for p in 1..numparts {
        if partsizes[p] >= partsizes[biggestpart] {
            biggestpart = p;
        }
    }
    merge_oldbl_parts(
        blobcoords,
        partids,
        partsizes,
        biggestpart as i32,
        jumplimit,
    );
    (biggestpart as i32, numparts)
}

fn merge_oldbl_parts(
    blobcoords: &[TBox],
    partids: &mut [i8],
    partsizes: &mut [i32; MAXPARTS],
    biggestpart: i32,
    jumplimit: f32,
) {
    let blobcount = blobcoords.len() as i32;
    let mut prevpart = biggestpart;
    let mut runlength = 0;
    let mut startx = 0i32;
    let centre = |i: i32| -> (f32, f32) {
        let b = blobcoords[i as usize];
        ((f64::from(b.left + b.right) / 2.0) as f32, b.bottom as f32)
    };
    for blobindex in 0..blobcount {
        if i32::from(partids[blobindex as usize]) != prevpart {
            if prevpart != biggestpart && runlength > MAXBADRUN {
                let mut stats = Qlsq::default();
                for t in startx..blobindex {
                    let (x, y) = centre(t);
                    stats.add(f64::from(x), f64::from(y));
                }
                stats.fit(1);
                let m = stats.b as f32;
                let c = stats.c as f32;
                let mut found_one = false;
                let mut close_one = false;
                let mut test_blob = 1;
                while !found_one && (startx - test_blob >= 0 || blobindex + test_blob <= blobcount)
                {
                    if startx - test_blob >= 0
                        && i32::from(partids[(startx - test_blob) as usize]) == biggestpart
                    {
                        found_one = true;
                        let (x, y) = centre(startx - test_blob);
                        let diff = m * x + c - y;
                        if diff < jumplimit && -diff < jumplimit {
                            close_one = true;
                        }
                    }
                    if blobindex + test_blob <= blobcount
                        && i32::from(partids[(blobindex + test_blob - 1) as usize]) == biggestpart
                    {
                        found_one = true;
                        let (x, y) = centre(blobindex + test_blob - 1);
                        let diff = m * x + c - y;
                        if diff < jumplimit && -diff < jumplimit {
                            close_one = true;
                        }
                    }
                    test_blob += 1;
                }
                if close_one {
                    partsizes[prevpart as usize] -= runlength;
                    for t in startx..blobindex {
                        partids[t as usize] = biggestpart as i8;
                    }
                }
            }
            prevpart = i32::from(partids[blobindex as usize]);
            runlength = 1;
            startx = blobindex;
        } else {
            runlength += 1;
        }
    }
}

fn get_ydiffs(blobcoords: &[TBox], spline: &QSpline, ydiffs: &mut [f32]) -> usize {
    let mut diffsum = 0.0f32;
    let mut bestindex = 0usize;
    let mut bestsum = i32::MAX as f32;
    let mut drift = 0.0f32;
    let mut lastx = blobcoords[0].left;
    for (blobindex, b) in blobcoords.iter().enumerate() {
        let xcentre = (b.left + b.right) >> 1;
        drift = (f64::from(drift) + spline.step(f64::from(lastx), f64::from(xcentre))) as f32;
        lastx = xcentre;
        let mut diff = b.bottom as f32;
        diff = (f64::from(diff) - spline.y(f64::from(xcentre))) as f32;
        diff += drift;
        ydiffs[blobindex] = diff;
        if blobindex > 2 {
            diffsum -= ydiffs[blobindex - 3].abs();
        }
        diffsum += diff.abs();
        if blobindex >= 2 && diffsum < bestsum {
            bestsum = diffsum;
            bestindex = blobindex - 1;
        }
    }
    bestindex
}

fn choose_partition(
    diff: f32,
    partdiffs: &mut [f32; MAXPARTS],
    mut lastpart: i32,
    jumplimit: f32,
    drift: &mut f32,
    lastdelta: &mut f32,
    partcount: &mut usize,
) -> i32 {
    if lastpart < 0 {
        partdiffs[0] = diff;
        lastpart = 0;
        *drift = 0.0;
        *lastdelta = 0.0;
    }
    let mut delta = diff - partdiffs[lastpart as usize] - *drift;
    let bestpart = if delta.abs() > jumplimit / 2.0 {
        let mut bestdelta = diff - partdiffs[0] - *drift;
        let mut bp = 0usize;
        for (partition, &pd) in partdiffs.iter().enumerate().take(*partcount).skip(1) {
            let d = diff - pd - *drift;
            if d.abs() < bestdelta.abs() {
                bestdelta = d;
                bp = partition;
            }
        }
        delta = bestdelta;
        if bestdelta.abs() > jumplimit && *partcount < MAXPARTS {
            bp = *partcount;
            *partcount += 1;
            partdiffs[bp] = diff - *drift;
            delta = 0.0;
        }
        bp as i32
    } else {
        lastpart
    };
    if bestpart == lastpart
        && ((delta - *lastdelta).abs() < jumplimit / 2.0 || delta.abs() < jumplimit / 2.0)
    {
        *drift = (3.0 * *drift + delta) / 3.0;
    }
    *lastdelta = delta;
    bestpart
}

fn partition_coords(
    blobcoords: &[TBox],
    partids: &[i8],
    bestpart: i32,
    xcoords: &mut [i32],
    ycoords: &mut [i32],
) -> usize {
    let mut pointcount = 0;
    for (i, b) in blobcoords.iter().enumerate() {
        if i32::from(partids[i]) == bestpart {
            xcoords[pointcount] = (b.left + b.right) >> 1;
            ycoords[pointcount] = b.bottom;
            pointcount += 1;
        }
    }
    pointcount
}

fn segment_spline(
    xcoords: &[i32],
    ycoords: &[i32],
    degree: i32,
    mut pointcount: usize,
    xstarts: &mut [i32; SPLINESIZE + 1],
) -> usize {
    let mut turnpoints = [0usize; SPLINESIZE];
    let mut turncount = 0usize;
    xstarts[0] = xcoords[0] - 1;
    let max_x = xcoords[pointcount - 1] + 1;
    if degree < 2 {
        pointcount = 0;
    }
    if pointcount > 3 {
        let mut ptindex = 1usize;
        let mut lastmax = 0usize;
        let mut lastmin = 0usize;
        while ptindex < pointcount - 1 && turncount < SPLINESIZE - 1 {
            if ycoords[ptindex - 1] > ycoords[ptindex] && ycoords[ptindex] <= ycoords[ptindex + 1] {
                if ycoords[ptindex] < ycoords[lastmax] - TURNLIMIT {
                    if turncount == 0 || turnpoints[turncount - 1] != lastmax {
                        turnpoints[turncount] = lastmax;
                        turncount += 1;
                    }
                    lastmin = ptindex;
                } else if ycoords[ptindex] < ycoords[lastmin] {
                    lastmin = ptindex;
                }
            }
            if ycoords[ptindex - 1] < ycoords[ptindex] && ycoords[ptindex] >= ycoords[ptindex + 1] {
                if ycoords[ptindex] > ycoords[lastmin] + TURNLIMIT {
                    if turncount == 0 || turnpoints[turncount - 1] != lastmin {
                        turnpoints[turncount] = lastmin;
                        turncount += 1;
                    }
                    lastmax = ptindex;
                } else if ycoords[ptindex] > ycoords[lastmax] {
                    lastmax = ptindex;
                }
            }
            ptindex += 1;
        }
        if ycoords[ptindex] < ycoords[lastmax] - TURNLIMIT
            && (turncount == 0 || turnpoints[turncount - 1] != lastmax)
        {
            if turncount < SPLINESIZE - 1 {
                turnpoints[turncount] = lastmax;
                turncount += 1;
            }
            if turncount < SPLINESIZE - 1 {
                turnpoints[turncount] = ptindex;
                turncount += 1;
            }
        } else if ycoords[ptindex] > ycoords[lastmin] + TURNLIMIT
            && (turncount == 0 || turnpoints[turncount - 1] != lastmin)
        {
            if turncount < SPLINESIZE - 1 {
                turnpoints[turncount] = lastmin;
                turncount += 1;
            }
            if turncount < SPLINESIZE - 1 {
                turnpoints[turncount] = ptindex;
                turncount += 1;
            }
        } else if turncount > 0
            && turnpoints[turncount - 1] == lastmin
            && turncount < SPLINESIZE - 1
        {
            if ycoords[ptindex] > ycoords[lastmax] {
                turnpoints[turncount] = ptindex;
            } else {
                turnpoints[turncount] = lastmax;
            }
            turncount += 1;
        } else if turncount > 0
            && turnpoints[turncount - 1] == lastmax
            && turncount < SPLINESIZE - 1
        {
            if ycoords[ptindex] < ycoords[lastmin] {
                turnpoints[turncount] = ptindex;
            } else {
                turnpoints[turncount] = lastmin;
            }
            turncount += 1;
        }
    }
    let mut segment = 1usize;
    while segment < turncount {
        let a = turnpoints[segment - 1];
        let b = turnpoints[segment];
        let centre = (ycoords[a] + ycoords[b]) / 2;
        let mut ptindex = a + 1;
        if ycoords[a] < ycoords[b] {
            while ptindex < b && ycoords[ptindex + 1] <= centre {
                ptindex += 1;
            }
        } else {
            while ptindex < b && ycoords[ptindex + 1] >= centre {
                ptindex += 1;
            }
        }
        xstarts[segment] =
            (xcoords[ptindex - 1] + xcoords[ptindex] + xcoords[a] + xcoords[b] + 2) / 4;
        segment += 1;
    }
    xstarts[segment] = max_x;
    segment
}

fn split_stepped_spline(
    baseline: &QSpline,
    jumplimit: f32,
    xcoords: &[i32],
    xstarts: &mut [i32; SPLINESIZE + 1],
    segments: &mut usize,
) -> bool {
    let medianwin = TEXTORD_SPLINE_MEDIANWIN as usize;
    let mut doneany = false;
    let mut startindex = 0usize;
    let mut segment = 1usize;
    while segment + 1 < *segments {
        let mut step = baseline.step(
            f64::from(xstarts[segment - 1] + xstarts[segment]) / 2.0,
            f64::from(xstarts[segment] + xstarts[segment + 1]) / 2.0,
        ) as f32;
        if step < 0.0 {
            step = -step;
        }
        if step > jumplimit {
            while xcoords[startindex] < xstarts[segment - 1] {
                startindex += 1;
            }
            let mut centreindex = startindex;
            while xcoords[centreindex] < xstarts[segment] {
                centreindex += 1;
            }
            let mut endindex = centreindex;
            while xcoords[endindex] < xstarts[segment + 1] {
                endindex += 1;
            }
            if *segments >= SPLINESIZE {
                // too many segments
            } else if endindex - startindex >= medianwin * 3 {
                while centreindex - startindex < medianwin * 3 / 2 {
                    centreindex += 1;
                }
                while endindex - centreindex < medianwin * 3 / 2 {
                    centreindex -= 1;
                }
                let mut leftindex = (startindex + startindex + centreindex) / 3;
                let mut rightindex = (centreindex + endindex + endindex) / 3;
                let leftcoord =
                    (f64::from(xcoords[startindex] * 2 + xcoords[centreindex]) / 3.0) as f32;
                let rightcoord =
                    (f64::from(xcoords[centreindex] + xcoords[endindex] * 2) / 3.0) as f32;
                while xcoords[leftindex] as f32 > leftcoord && leftindex - startindex > medianwin {
                    leftindex -= 1;
                }
                while (xcoords[leftindex] as f32) < leftcoord
                    && centreindex - leftindex > medianwin / 2
                {
                    leftindex += 1;
                }
                if xcoords[leftindex] as f32 - leftcoord > leftcoord - xcoords[leftindex - 1] as f32
                {
                    leftindex -= 1;
                }
                while xcoords[rightindex] as f32 > rightcoord
                    && rightindex - centreindex > medianwin / 2
                {
                    rightindex -= 1;
                }
                while (xcoords[rightindex] as f32) < rightcoord && endindex - rightindex > medianwin
                {
                    rightindex += 1;
                }
                if xcoords[rightindex] as f32 - rightcoord
                    > rightcoord - xcoords[rightindex - 1] as f32
                {
                    rightindex -= 1;
                }
                let c1 = (xcoords[leftindex - 1] + xcoords[leftindex]) / 2;
                let c2 = (xcoords[rightindex - 1] + xcoords[rightindex]) / 2;
                // insert_spline_point
                let mut index = *segments;
                while index > segment {
                    xstarts[index + 1] = xstarts[index];
                    index -= 1;
                }
                *segments += 1;
                xstarts[segment] = c1;
                xstarts[segment + 1] = c2;
                doneany = true;
            }
        }
        segment += 1;
    }
    doneany
}

fn find_lesser_parts(
    row: &mut ToRow,
    blobcoords: &[TBox],
    partids: &[i8],
    partsizes: &[i32; MAXPARTS],
    partcount: usize,
    bestpart: i32,
) {
    let mut partsteps = [0f32; MAXPARTS];
    let mut runlength = 0;
    let mut biggestrun = 0;
    for (i, b) in blobcoords.iter().enumerate() {
        let xcentre = (b.left + b.right) >> 1;
        let part_id = i32::from(partids[i] as u8);
        if part_id != bestpart {
            runlength += 1;
            if runlength > biggestrun {
                biggestrun = runlength;
            }
            let p = &mut partsteps[part_id as usize];
            *p =
                (f64::from(*p) + (f64::from(b.bottom) - row.baseline.y(f64::from(xcentre)))) as f32;
        } else {
            runlength = 0;
        }
    }
    row.xheight = if biggestrun > MAXBADRUN { -1.0 } else { 1.0 };
    let mut poscount = 0;
    let mut negcount = 0;
    let mut bestneg = 0.0f32;
    for partition in 0..partcount {
        if partition as i32 != bestpart {
            if partsizes[partition] == 0 {
                partsteps[partition] = 0.0;
            } else {
                partsteps[partition] /= partsizes[partition] as f32;
            }
            if partsteps[partition] >= MINASCRISE && partsizes[partition] > poscount {
                poscount = partsizes[partition];
            }
            if partsteps[partition] <= -MINASCRISE && partsizes[partition] > negcount {
                bestneg = partsteps[partition];
                negcount = partsizes[partition];
            }
        }
    }
    row.descdrop = bestneg;
}

fn get_min_max_xheight(block_linesize: i32) -> (i32, i32) {
    let mut min_height = (f64::from(block_linesize) * TEXTORD_MINXH).floor() as i32;
    if min_height < TEXTORD_MIN_XHEIGHT {
        min_height = TEXTORD_MIN_XHEIGHT;
    }
    let max_height = (f64::from(block_linesize) * 3.0).ceil() as i32;
    (min_height, max_height)
}

#[derive(PartialEq, Eq)]
enum RowCategory {
    Ascenders,
    Descenders,
    Unknown,
    Invalid,
}

fn get_row_category(row: &ToRow) -> RowCategory {
    if row.xheight <= 0.0 {
        return RowCategory::Invalid;
    }
    if row.ascrise > 0.0 {
        RowCategory::Ascenders
    } else if row.descdrop != 0.0 {
        RowCategory::Descenders
    } else {
        RowCategory::Unknown
    }
}

fn within_error_margin(test: f32, num: f32, margin: f32) -> bool {
    test >= num * (1.0 - margin) && test <= num * (1.0 + margin)
}

/// `Textord::compute_row_xheight`.
fn compute_row_xheight(row: &mut ToRow, gradient: f32, block_line_size: i32, blobs: &mut Blobs) {
    if !row.rep_chars_marked() {
        mark_repeated_chars(row, blobs);
    }
    let (min_height, max_height) = get_min_max_xheight(block_line_size);
    let mut heights = Stats::new(min_height, max_height);
    let mut floating = Stats::new(min_height, max_height);
    fill_heights(
        row,
        blobs,
        min_height,
        max_height,
        &mut heights,
        &mut floating,
    );
    row.ascrise = 0.0;
    row.xheight = 0.0;
    let (mut xh, mut asc) = (row.xheight, row.ascrise);
    row.xheight_evidence = compute_xheight_from_modes(
        &mut heights,
        &floating,
        min_height,
        max_height,
        &mut xh,
        &mut asc,
    );
    row.xheight = xh;
    row.ascrise = asc;
    row.descdrop = 0.0;
    if row.xheight > 0.0 {
        row.descdrop =
            compute_row_descdrop(row, gradient, row.xheight_evidence, &heights, blobs) as f32;
    }
}

fn fill_heights(
    row: &ToRow,
    blobs: &Blobs,
    min_height: i32,
    max_height: i32,
    heights: &mut Stats,
    floating: &mut Stats,
) {
    let list = &row.blobs;
    if list.is_empty() {
        return;
    }
    let has_rep_chars = row.rep_chars_marked() && row.num_repeated_sets > 0;
    let mut it = Iter::new(list);
    loop {
        let b = blobs.get(it.data(list));
        if !b.joined {
            let xcentre = (b.bbox.left + b.bbox.right) as f32 / 2.0;
            let mut top = b.bbox.top as f32;
            let height = b.bbox.height() as f32;
            top = (f64::from(top) - row.baseline.y(f64::from(xcentre))) as f32;
            if top >= min_height as f32 && top <= max_height as f32 {
                let v = (f64::from(top) + 0.5).floor() as i32;
                heights.add(v, 1);
                if f64::from(height / top) < TEXTORD_MIN_BLOB_HEIGHT_FRACTION {
                    floating.add(v, 1);
                }
            }
        }
        if has_rep_chars && b.repeated_set != 0 {
            let set = b.repeated_set;
            it.forward(list);
            while !it.at_first(list) && blobs.get(it.data(list)).repeated_set == set {
                it.forward(list);
            }
        } else {
            it.forward(list);
        }
        if it.at_first(list) {
            break;
        }
    }
}

fn compute_xheight_from_modes(
    heights: &mut Stats,
    floating: &Stats,
    min_height: i32,
    max_height: i32,
    xheight: &mut f32,
    ascrise: &mut f32,
) -> i32 {
    let mut blob_index = heights.mode();
    let blob_count = heights.pile_count(blob_index);
    if blob_count == 0 {
        return 0;
    }
    let mut modes = [0i32; MAX_HEIGHT_MODES];
    let mut in_best_pile = false;
    let mut prev_size = -i32::MAX;
    let mut best_count = 0;
    let mode_count = compute_height_modes(heights, min_height, max_height, &mut modes);
    for x in 0..mode_count.saturating_sub(1) {
        if modes[x] != prev_size + 1 {
            in_best_pile = false;
        }
        let modes_x_count = heights.pile_count(modes[x]) - floating.pile_count(modes[x]);
        if f64::from(modes_x_count) >= f64::from(blob_count) * TEXTORD_XHEIGHT_MODE_FRACTION
            && (in_best_pile || modes_x_count > best_count)
        {
            for asc in x + 1..mode_count {
                let ratio = modes[asc] as f32 / modes[x] as f32;
                if TEXTORD_ASCX_RATIO_MIN < f64::from(ratio)
                    && f64::from(ratio) < TEXTORD_ASCX_RATIO_MAX
                    && f64::from(heights.pile_count(modes[asc]))
                        >= f64::from(blob_count) * TEXTORD_ASCHEIGHT_MODE_FRACTION
                {
                    if modes_x_count > best_count {
                        in_best_pile = true;
                        best_count = modes_x_count;
                    }
                    prev_size = modes[x];
                    *xheight = modes[x] as f32;
                    *ascrise = (modes[asc] - modes[x]) as f32;
                }
            }
        }
    }
    if *xheight == 0.0 {
        if floating.get_total() > 0 {
            for x in min_height..max_height {
                heights.add(x, -floating.pile_count(x));
            }
            blob_index = heights.mode();
            for x in min_height..max_height {
                heights.add(x, floating.pile_count(x));
            }
        }
        *xheight = blob_index as f32;
        *ascrise = 0.0;
        best_count = heights.pile_count(blob_index);
    }
    best_count
}

fn compute_row_descdrop(
    row: &ToRow,
    gradient: f32,
    xheight_blob_count: i32,
    asc_heights: &Stats,
    blobs: &Blobs,
) -> i32 {
    let mut i_min = asc_heights.min_bucket();
    if f64::from(i_min as f32 / row.xheight) < TEXTORD_ASCX_RATIO_MIN {
        i_min = (f64::from(row.xheight) * TEXTORD_ASCX_RATIO_MIN + 0.5).floor() as i32;
    }
    let mut i_max = asc_heights.max_bucket();
    if f64::from(i_max as f32 / row.xheight) > TEXTORD_ASCX_RATIO_MAX {
        i_max = (f64::from(row.xheight) * TEXTORD_ASCX_RATIO_MAX).floor() as i32;
    }
    let mut num_potential_asc = 0;
    for i in i_min..=i_max {
        num_potential_asc += asc_heights.pile_count(i);
    }
    let min_height = (f64::from(row.xheight) * TEXTORD_DESCX_RATIO_MIN + 0.5).floor() as i32;
    let max_height = (f64::from(row.xheight) * TEXTORD_DESCX_RATIO_MAX).floor() as i32;
    let mut heights = Stats::new(min_height, max_height);
    for id in row.blobs.to_vec() {
        let b = blobs.get(id);
        if !b.joined {
            let xcentre = (b.bbox.left + b.bbox.right) as f32 / 2.0;
            let height = gradient * xcentre + row.para_c - b.bbox.bottom as f32;
            if height >= min_height as f32 && height <= max_height as f32 {
                heights.add((f64::from(height) + 0.5).floor() as i32, 1);
            }
        }
    }
    let blob_index = heights.mode();
    let mut blob_count = heights.pile_count(blob_index);
    let total_fraction =
        (TEXTORD_DESCHEIGHT_MODE_FRACTION + TEXTORD_ASCHEIGHT_MODE_FRACTION) as f32;
    if ((blob_count + num_potential_asc) as f32) < xheight_blob_count as f32 * total_fraction {
        blob_count = 0;
    }
    if blob_count > 0 { -blob_index } else { 0 }
}

fn compute_height_modes(
    heights: &Stats,
    min_height: i32,
    max_height: i32,
    modes: &mut [i32; MAX_HEIGHT_MODES],
) -> usize {
    let maxmodes = MAX_HEIGHT_MODES;
    let src_count = max_height + 1 - min_height;
    let mut dest_count = 0usize;
    let mut least_count = i32::MAX;
    let mut least_index: isize = -1;
    for src_index in 0..src_count {
        let pile_count = heights.pile_count(min_height + src_index);
        if pile_count > 0 {
            if dest_count < maxmodes {
                if pile_count < least_count {
                    least_count = pile_count;
                    least_index = dest_count as isize;
                }
                modes[dest_count] = min_height + src_index;
                dest_count += 1;
            } else if pile_count >= least_count {
                while least_index < maxmodes as isize - 1 {
                    modes[least_index as usize] = modes[least_index as usize + 1];
                    least_index += 1;
                }
                modes[maxmodes - 1] = min_height + src_index;
                if pile_count == least_count {
                    least_index = maxmodes as isize - 1;
                } else {
                    least_count = heights.pile_count(modes[0]);
                    least_index = 0;
                    for (d, &m) in modes.iter().enumerate().skip(1) {
                        let pc = heights.pile_count(m);
                        if pc < least_count {
                            least_count = pc;
                            least_index = d as isize;
                        }
                    }
                    dest_count = maxmodes;
                }
            }
        }
    }
    dest_count
}

fn compute_block_xheight(block: &mut ToBlk, blobs: &mut Blobs, gradient: f32) {
    let asc_frac_xheight = (K_ASCENDER_FRACTION / K_XHEIGHT_FRACTION) as f32;
    let desc_frac_xheight = (K_DESCENDER_FRACTION / K_XHEIGHT_FRACTION) as f32;
    if block.rows.is_empty() {
        return;
    }
    let (min_height, max_height) = get_min_max_xheight(block.line_size as i32);
    let mut row_asc_xheights = Stats::new(min_height, max_height);
    let mut row_asc_ascrise = Stats::new(
        (min_height as f32 * asc_frac_xheight) as i32,
        (max_height as f32 * asc_frac_xheight) as i32,
    );
    let min_desc_height = (min_height as f32 * desc_frac_xheight) as i32;
    let max_desc_height = (max_height as f32 * desc_frac_xheight) as i32;
    let mut row_asc_descdrop = Stats::new(min_desc_height, max_desc_height);
    let mut row_desc_xheights = Stats::new(min_height, max_height);
    let mut row_desc_descdrop = Stats::new(min_desc_height, max_desc_height);
    let mut row_cap_xheights = Stats::new(min_height, max_height);
    let mut row_cap_floating_xheights = Stats::new(min_height, max_height);
    let line_size = block.line_size as i32;
    for row in &mut block.rows {
        if row.xheight <= 0.0 {
            compute_row_xheight(row, gradient, line_size, blobs);
        }
        match get_row_category(row) {
            RowCategory::Ascenders => {
                row_asc_xheights.add(row.xheight as i32, row.xheight_evidence);
                row_asc_ascrise.add(row.ascrise as i32, row.xheight_evidence);
                row_asc_descdrop.add((-row.descdrop) as i32, row.xheight_evidence);
            }
            RowCategory::Descenders => {
                row_desc_xheights.add(row.xheight as i32, row.xheight_evidence);
                row_desc_descdrop.add((-row.descdrop) as i32, row.xheight_evidence);
            }
            RowCategory::Unknown => {
                fill_heights(
                    row,
                    blobs,
                    min_height,
                    max_height,
                    &mut row_cap_xheights,
                    &mut row_cap_floating_xheights,
                );
            }
            RowCategory::Invalid => {}
        }
    }
    let mut xheight = 0.0f32;
    let mut ascrise = 0.0f32;
    let mut descdrop = 0.0f32;
    if row_asc_xheights.get_total() > 0 {
        xheight = row_asc_xheights.median() as f32;
        ascrise = row_asc_ascrise.median() as f32;
        descdrop = -row_asc_descdrop.median() as f32;
    } else if row_desc_xheights.get_total() > 0 {
        xheight = row_desc_xheights.median() as f32;
        descdrop = -row_desc_descdrop.median() as f32;
    } else if row_cap_xheights.get_total() > 0 {
        compute_xheight_from_modes(
            &mut row_cap_xheights,
            &row_cap_floating_xheights,
            min_height,
            max_height,
            &mut xheight,
            &mut ascrise,
        );
        if ascrise == 0.0 {
            xheight = (row_cap_xheights.median() * K_XHEIGHT_CAP_RATIO) as f32;
        }
    } else {
        xheight = (f64::from(block.line_size) * K_XHEIGHT_FRACTION) as f32;
    }
    let mut corrected = false;
    if xheight < TEXTORD_MIN_XHEIGHT as f32 {
        xheight = TEXTORD_MIN_XHEIGHT as f32;
        corrected = true;
    }
    if corrected || ascrise <= 0.0 {
        ascrise = xheight * asc_frac_xheight;
    }
    if corrected || descdrop >= 0.0 {
        descdrop = -(xheight * desc_frac_xheight);
    }
    block.xheight = xheight;
    for row in &mut block.rows {
        correct_row_xheight(row, xheight, ascrise, descdrop);
    }
}

fn correct_row_xheight(row: &mut ToRow, xheight: f32, ascrise: f32, descdrop: f32) {
    let cat = get_row_category(row);
    let margin = TEXTORD_XHEIGHT_ERROR_MARGIN as f32;
    let normal_xheight = within_error_margin(row.xheight, xheight, margin);
    let cap_xheight = within_error_margin(row.xheight, xheight + ascrise, margin);
    if cat == RowCategory::Ascenders {
        if row.descdrop >= 0.0 {
            row.descdrop = row.xheight * (descdrop / xheight);
        }
    } else if cat == RowCategory::Invalid
        || (cat == RowCategory::Descenders && (normal_xheight || cap_xheight))
        || (cat == RowCategory::Unknown && normal_xheight)
    {
        row.xheight = xheight;
        row.ascrise = ascrise;
        row.descdrop = descdrop;
    } else if cat == RowCategory::Descenders {
        row.ascrise = row.xheight * (ascrise / xheight);
    } else if cat == RowCategory::Unknown {
        row.all_caps = true;
        if cap_xheight {
            row.xheight = xheight;
            row.ascrise = ascrise;
            row.descdrop = descdrop;
        } else {
            row.ascrise = row.xheight * (ascrise / (xheight + ascrise));
            row.xheight -= row.ascrise;
            row.descdrop = row.xheight * (descdrop / xheight);
        }
    }
}

/// `mark_repeated_chars` (including its habit of resetting the last
/// examined blob rather than the current one).
fn mark_repeated_chars(row: &mut ToRow, blobs: &mut Blobs) {
    let mut marks = Vec::new();
    let mut num_repeated_sets = 0;
    let list = &row.blobs;
    if !list.is_empty() {
        let mut box_it = Iter::new(list);
        loop {
            let mut bblob = box_it.data(list);
            let mut repeat_length = 1;
            let b = blobs.get(bblob);
            if b.flow == FlowType::Leader && !b.joined && b.cblob.is_some() {
                let mut test_it = box_it;
                test_it.forward(list);
                while !test_it.at_first(list) {
                    bblob = test_it.data(list);
                    if blobs.get(bblob).flow != FlowType::Leader {
                        break;
                    }
                    test_it.forward(list);
                    bblob = test_it.data(list);
                    let tb = blobs.get(bblob);
                    if tb.joined || tb.cblob.is_none() {
                        repeat_length = 0;
                        break;
                    }
                    repeat_length += 1;
                }
            }
            if repeat_length >= MIN_LEADER_COUNT {
                num_repeated_sets += 1;
                while repeat_length > 0 {
                    let id = box_it.data(list);
                    marks.push((id, num_repeated_sets));
                    box_it.forward(list);
                    repeat_length -= 1;
                }
            } else {
                marks.push((bblob, 0));
                box_it.forward(list);
            }
            if box_it.at_first(list) {
                break;
            }
        }
    }
    for (id, set) in marks {
        blobs.get_mut(id).repeated_set = set;
    }
    row.num_repeated_sets = num_repeated_sets;
}

/// `restore_underlined_blobs`.
fn restore_underlined_blobs(block: &mut ToBlk, blobs: &mut Blobs) {
    if block.rows.is_empty() {
        return;
    }
    let mut residual: Vec<BlobId> = Vec::new();
    let mut arena = OutlineArena::default();
    let mut left: EList<u32> = EList::new();
    let mut right: EList<u32> = EList::new();
    let pitch_error = (f64::from(TEXTORD_FP_CHOP_ERROR) + 0.5) as f32;
    let ToBlk {
        underlines: list,
        rows,
        ..
    } = block;
    let mut under_it = Iter::new(list);
    under_it.mark_cycle_pt();
    while !under_it.cycled_list(list) {
        let mut u_line = Some(under_it.extract(list));
        let blob_box = blobs.get(u_line.expect("u_line")).bbox;
        let Some(ri) = most_overlapping_row(rows, &blob_box) else {
            return;
        };
        let row = &rows[ri];
        let cells = find_underlined_blobs(
            blobs
                .get(u_line.expect("u_line"))
                .cblob
                .as_ref()
                .expect("underline cblob"),
            &blob_box,
            &row.baseline,
            row.xheight,
            (f64::from(row.xheight) * TEXTORD_UNDERLINE_OFFSET) as f32,
        );
        for (cx, cy) in cells {
            let mut chop_coord = cx;
            if cy - chop_coord > TEXTORD_FP_CHOP_ERROR + 1 {
                let cb = u_line.take().and_then(|id| blobs.get_mut(id).cblob.take());
                split_to_blob(
                    cb,
                    chop_coord as i16,
                    pitch_error,
                    &mut left,
                    &mut right,
                    &mut arena,
                );
                if !left.is_empty() {
                    let cb = arena.take_blob(&mut left);
                    residual.push(blobs.add(BlobNBox::new(cb)));
                }
                chop_coord = cy;
                split_to_blob(
                    None,
                    chop_coord as i16,
                    pitch_error,
                    &mut left,
                    &mut right,
                    &mut arena,
                );
                if !left.is_empty() {
                    let cb = arena.take_blob(&mut left);
                    let nb = blobs.add(BlobNBox::new(cb));
                    rows[ri].insert_blob(blobs, nb);
                }
            }
        }
        if !right.is_empty() {
            split_to_blob(
                None,
                blob_box.right as i16,
                pitch_error,
                &mut left,
                &mut right,
                &mut arena,
            );
            if !left.is_empty() {
                let cb = arena.take_blob(&mut left);
                residual.push(blobs.add(BlobNBox::new(cb)));
            }
        }
        under_it.forward(list);
    }
    for id in residual {
        let mut it = Iter::new(list);
        it.move_to_last(list);
        it.add_after_then_move(list, id);
    }
}

fn most_overlapping_row(rows: &[ToRow], bbox: &TBox) -> Option<usize> {
    let x = f64::from(((bbox.left + bbox.right) / 2) as i16);
    if rows.is_empty() {
        return None;
    }
    let mut best_row = None;
    let mut bestover = -i32::MAX as f32;
    let n = rows.len();
    let mut ri = 0usize;
    let mut cycled = false;
    let next = |ri: &mut usize, cycled: &mut bool| {
        *ri = (*ri + 1) % n;
        if *ri == 0 {
            *cycled = true;
        }
    };
    while rows[ri].baseline.y(x) + f64::from(rows[ri].descdrop) > f64::from(bbox.top) && !cycled {
        best_row = Some(ri);
        bestover =
            (f64::from(bbox.top) - rows[ri].baseline.y(x) + f64::from(rows[ri].descdrop)) as f32;
        next(&mut ri, &mut cycled);
    }
    while rows[ri].baseline.y(x) + f64::from(rows[ri].xheight) + f64::from(rows[ri].ascrise)
        >= f64::from(bbox.bottom)
        && !cycled
    {
        let r = &rows[ri];
        let mut overlap = (r.baseline.y(x) + f64::from(r.xheight) + f64::from(r.ascrise)) as f32;
        if (bbox.top as f32) < overlap {
            overlap = bbox.top as f32;
        }
        if f64::from(bbox.bottom) > r.baseline.y(x) + f64::from(r.descdrop) {
            overlap -= bbox.bottom as f32;
        } else {
            overlap = (f64::from(overlap) - (r.baseline.y(x) + f64::from(r.descdrop))) as f32;
        }
        if overlap > bestover {
            bestover = overlap;
            best_row = Some(ri);
        }
        next(&mut ri, &mut cycled);
    }
    let r = &rows[ri];
    if bestover < 0.0
        && r.baseline.y(x) + f64::from(r.xheight) + f64::from(r.ascrise) - f64::from(bbox.bottom)
            > f64::from(bestover)
    {
        best_row = Some(ri);
    }
    best_row
}

/// `find_underlined_blobs`: the chop cells (start, end) in x.
fn find_underlined_blobs(
    cblob: &CBlob,
    blob_box: &TBox,
    baseline: &QSpline,
    xheight: f32,
    baseline_offset: f32,
) -> Vec<(i32, i32)> {
    let mut upper = Stats::new(blob_box.left, blob_box.right);
    let mut middle = Stats::new(blob_box.left, blob_box.right);
    let mut lower = Stats::new(blob_box.left, blob_box.right);
    for o in &cblob.outlines {
        vertical_cunderline_projection(
            o,
            baseline,
            xheight,
            baseline_offset,
            &mut lower,
            &mut middle,
            &mut upper,
        );
    }
    let mut cells = Vec::new();
    let mut x = blob_box.left;
    while x < blob_box.right {
        if middle.pile_count(x) > 0 {
            let mut y = x + 1;
            while y < blob_box.right && middle.pile_count(y) > 0 {
                y += 1;
            }
            cells.push((x, y));
            x = y;
        }
        x += 1;
    }
    cells
}

fn vertical_cunderline_projection(
    o: &super::outline::Outline,
    baseline: &QSpline,
    xheight: f32,
    baseline_offset: f32,
    lower: &mut Stats,
    middle: &mut Stats,
    upper: &mut Stats,
) {
    let mut pos = o.start;
    let off = f64::from(baseline_offset);
    for s in 0..o.steps.len() {
        let st = o.step(s);
        if st.x > 0 {
            let by = baseline.y(f64::from(pos.x));
            let lower_y = i32::from((by + off + 0.5).floor() as i16);
            let upper_y = i32::from((by + off + f64::from(xheight) + 0.5).floor() as i16);
            if pos.y >= lower_y {
                lower.add(pos.x, -lower_y);
                if pos.y >= upper_y {
                    middle.add(pos.x, lower_y - upper_y);
                    upper.add(pos.x, upper_y - pos.y);
                } else {
                    middle.add(pos.x, lower_y - pos.y);
                }
            } else {
                lower.add(pos.x, -pos.y);
            }
        } else if st.x < 0 {
            let by = baseline.y(f64::from(pos.x - 1));
            let lower_y = i32::from((by + off + 0.5).floor() as i16);
            let upper_y = i32::from((by + off + f64::from(xheight) + 0.5).floor() as i16);
            if pos.y >= lower_y {
                lower.add(pos.x - 1, lower_y);
                if pos.y >= upper_y {
                    middle.add(pos.x - 1, upper_y - lower_y);
                    upper.add(pos.x - 1, pos.y - upper_y);
                } else {
                    middle.add(pos.x - 1, pos.y - lower_y);
                }
            } else {
                lower.add(pos.x - 1, pos.y);
            }
        }
        pos += st;
    }
    for c in &o.children {
        vertical_cunderline_projection(c, baseline, xheight, baseline_offset, lower, middle, upper);
    }
}

/// `mark_repeated_chars` for word segmentation.
pub fn mark_repeated(row: &mut ToRow, blobs: &mut Blobs) {
    mark_repeated_chars(row, blobs);
}
