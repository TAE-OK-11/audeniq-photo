//! `TabFind`: tab-stop detection on a blob grid (`tabfind.cpp`), the part
//! the sparse-text layout path uses.

use super::alignedblob::{AlignedBlobParams, find_vertical_alignment};
use super::blobbox::{BlobId, Blobs, FlowType, RegionType, TabType, ToBlock};
use super::detlinefit::int_cast_rounded;
use super::elist::{EList, Iter};
use super::geom::{ICoord, TBox};
use super::grid::{BBGrid, GridSearch};
use super::linefind::shrink;
use super::stats::Stats;
use super::stdalgo;
use super::tabvector::{TabAlignment, TabVector, TabVectors, VecId};
use std::collections::HashMap;

const TAB_RADIUS_FACTOR: i32 = 5;
const MIN_VERTICAL_SEARCH: i32 = 3;
const MAX_VERTICAL_SEARCH: i32 = 12;
const MAX_RAGGED_SEARCH: i32 = 25;
const ALIGNED_FRACTION: f64 = 0.03125;
const RAGGED_GUTTER_MULTIPLE: i32 = 5;
const LINE_FRAGMENT_ASPECT_RATIO: f64 = 10.0;
const MIN_EVALUATED_TABS: usize = 3;

const GUTTER_MULTIPLE: i32 = 4;
const GUTTER_TO_NEIGHBOUR_RATIO: i32 = 3;
const MAX_FILLIN_MULTIPLE: f64 = 11.0;
const MIN_GUTTER_FRACTION: f64 = 0.5;
const LINE_COUNT_RECIPROCAL: f64 = 4.0;
const MIN_ALIGNED_GUTTER: f64 = 0.25;
const MIN_RAGGED_GUTTER: f64 = 1.5;

pub fn nearly_equal(a: i32, b: i32, tolerance: i32) -> bool {
    (a - b).abs() <= tolerance
}

pub struct TabFind {
    pub grid: BBGrid<BlobId>,
    pub resolution: i32,
    pub vectors: EList<VecId>,
    v_it: Iter,
    pub vertical_skew: ICoord,
    pub tv: TabVectors,
    left_tab_boxes: Vec<BlobId>,
    right_tab_boxes: Vec<BlobId>,
}

impl TabFind {
    pub fn new(
        gridsize: i32,
        bleft: ICoord,
        tright: ICoord,
        vertical_x: i32,
        vertical_y: i32,
        resolution: i32,
    ) -> TabFind {
        let vectors = EList::new();
        let v_it = Iter::new(&vectors);
        let mut tf = TabFind {
            grid: BBGrid::new(gridsize, bleft, tright),
            resolution,
            vectors,
            v_it,
            vertical_skew: ICoord::default(),
            tv: TabVectors::default(),
            left_tab_boxes: Vec::new(),
            right_tab_boxes: Vec::new(),
        };
        tf.set_vertical_skew_and_parallelize(&Blobs::default(), vertical_x, vertical_y);
        tf
    }

    pub fn gridsize(&self) -> i32 {
        self.grid.base.gridsize
    }

    pub fn bleft(&self) -> ICoord {
        self.grid.base.bleft
    }

    pub fn tright(&self) -> ICoord {
        self.grid.base.tright
    }

    pub fn insert_blobs_to_grid(
        &mut self,
        blobs: &mut Blobs,
        h_spread: bool,
        v_spread: bool,
        list: &EList<BlobId>,
    ) {
        for b in list.to_vec() {
            self.insert_blob(blobs, h_spread, v_spread, b);
        }
    }

    pub fn insert_blob(
        &mut self,
        blobs: &mut Blobs,
        h_spread: bool,
        v_spread: bool,
        b: BlobId,
    ) -> bool {
        self.set_rules(blobs, b);
        if blobs.get(b).joined {
            return false;
        }
        self.grid.insert_bbox(&*blobs, h_spread, v_spread, b);
        true
    }

    fn set_rules(&mut self, blobs: &mut Blobs, b: BlobId) {
        let bx = blobs.get(b).bbox;
        let lr = self.left_edge_for_box(&bx, false, false);
        let rr = self.right_edge_for_box(&bx, false, false);
        let lc = self.left_edge_for_box(&bx, true, false);
        let rc = self.right_edge_for_box(&bx, true, false);
        let bb = blobs.get_mut(b);
        bb.left_rule = lr;
        bb.right_rule = rr;
        bb.left_crossing_rule = lc;
        bb.right_crossing_rule = rc;
    }

    pub fn set_block_rule_edges(&mut self, blobs: &mut Blobs, tb: &ToBlock) {
        for list in [&tb.blobs, &tb.small_blobs, &tb.noise_blobs, &tb.large_blobs] {
            self.set_blob_rule_edges(blobs, list);
        }
    }

    pub fn set_blob_rule_edges(&mut self, blobs: &mut Blobs, list: &EList<BlobId>) {
        for b in list.to_vec() {
            self.set_rules(blobs, b);
        }
    }

    /// `GutterWidth`: returns (gutter, required_shift).
    pub fn gutter_width(
        &self,
        blobs: &Blobs,
        bottom_y: i32,
        top_y: i32,
        v: &TabVector,
        ignore_unmergeables: bool,
        max_gutter_width: i32,
    ) -> (i32, i32) {
        let right_to_left = v.is_left_tab();
        let bottom_x = v.x_at_y(bottom_y);
        let top_x = v.x_at_y(top_y);
        let start_x = if right_to_left {
            top_x.max(bottom_x)
        } else {
            top_x.min(bottom_x)
        };
        let mut s = GridSearch::new();
        s.start_side_search(&self.grid, start_x, bottom_y, top_y);
        let mut min_gap = max_gutter_width;
        let mut required_shift = 0;
        let gs = self.gridsize();
        while let Some(b) = s.next_side_search(&self.grid, right_to_left) {
            let bb = blobs.get(b);
            let bx = bb.bbox;
            if bx.bottom >= top_y || bx.top <= bottom_y {
                continue;
            }
            if bx.height() >= gs * 2
                && f64::from(bx.height()) > f64::from(bx.width()) * LINE_FRAGMENT_ASPECT_RATIO
            {
                continue;
            }
            if ignore_unmergeables && unmergeable(bb.region_type) {
                continue;
            }
            let mid_y = (bx.bottom + bx.top) / 2;
            let tab_x = v.x_at_y(mid_y);
            let gap;
            if right_to_left {
                gap = tab_x - bx.right;
                if gap < 0 && bx.left - tab_x < required_shift {
                    required_shift = bx.left - tab_x;
                }
            } else {
                gap = bx.left - tab_x;
                if gap < 0 && bx.right - tab_x > required_shift {
                    required_shift = bx.right - tab_x;
                }
            }
            if gap > 0 && gap < min_gap {
                min_gap = gap;
            }
        }
        (min_gap - required_shift.abs(), required_shift)
    }

    /// `GutterWidthAndNeighbourGap`: returns (gutter_width, neighbour_gap).
    pub fn gutter_width_and_neighbour_gap(
        &mut self,
        blobs: &Blobs,
        tab_x: i32,
        _mean_height: i32,
        max_gutter: i32,
        left: bool,
        bbox: BlobId,
    ) -> (i32, i32) {
        let bx = blobs.get(bbox).bbox;
        let gutter_x = if left { bx.left } else { bx.right };
        let internal_x = if left { bx.right } else { bx.left };
        let tab_gap = if left {
            gutter_x - tab_x
        } else {
            tab_x - gutter_x
        };
        let mut gutter_width = max_gutter;
        if tab_gap > 0 {
            gutter_width += tab_gap;
        }
        let on_image = blobs.get(bbox).flow == FlowType::TextOnImage;
        if let Some(g) = self.adjacent_blob(
            blobs,
            bbox,
            left,
            on_image,
            0.0,
            gutter_width,
            bx.top,
            bx.bottom,
        ) {
            let gb = blobs.get(g).bbox;
            gutter_width = if left {
                tab_x - gb.right
            } else {
                gb.left - tab_x
            };
        }
        if gutter_width >= max_gutter {
            let mut gutter_box = bx;
            if left {
                gutter_box.left = tab_x - max_gutter - 1;
                gutter_box.right = tab_x - max_gutter;
                let tab_gutter = self.right_edge_for_box(&gutter_box, true, false);
                if tab_gutter < tab_x - 1 {
                    gutter_width = tab_x - tab_gutter;
                }
            } else {
                gutter_box.left = tab_x + max_gutter;
                gutter_box.right = tab_x + max_gutter + 1;
                let tab_gutter = self.left_edge_for_box(&gutter_box, true, false);
                if tab_gutter > tab_x + 1 {
                    gutter_width = tab_gutter - tab_x;
                }
            }
        }
        if gutter_width > max_gutter {
            gutter_width = max_gutter;
        }
        let neighbour = self.adjacent_blob(
            blobs,
            bbox,
            !left,
            on_image,
            0.0,
            gutter_width,
            bx.top,
            bx.bottom,
        );
        let mut neighbour_edge = if left {
            self.right_edge_for_box(&bx, true, false)
        } else {
            self.left_edge_for_box(&bx, true, false)
        };
        if let Some(n) = neighbour {
            let nb = blobs.get(n).bbox;
            if left && nb.left < neighbour_edge {
                neighbour_edge = nb.left;
            } else if !left && nb.right > neighbour_edge {
                neighbour_edge = nb.right;
            }
        }
        let neighbour_gap = if left {
            neighbour_edge - internal_x
        } else {
            internal_x - neighbour_edge
        };
        (gutter_width, neighbour_gap)
    }

    pub fn right_edge_for_box(&mut self, b: &TBox, crossing: bool, extended: bool) -> i32 {
        match self.right_tab_for_box(b, crossing, extended) {
            None => self.tright().x,
            Some(v) => self.tv.get(v).x_at_y((b.top + b.bottom) / 2),
        }
    }

    pub fn left_edge_for_box(&mut self, b: &TBox, crossing: bool, extended: bool) -> i32 {
        match self.left_tab_for_box(b, crossing, extended) {
            None => self.bleft().x,
            Some(v) => self.tv.get(v).x_at_y((b.top + b.bottom) / 2),
        }
    }

    fn key(&self, v: VecId) -> i32 {
        self.tv.get(v).sort_key
    }

    pub fn right_tab_for_box(&mut self, b: &TBox, crossing: bool, extended: bool) -> Option<VecId> {
        let l = &self.vectors;
        if l.is_empty() {
            return None;
        }
        let (top_y, bottom_y) = (b.top, b.bottom);
        let mid_y = (top_y + bottom_y) / 2;
        let right = if crossing {
            (b.left + b.right) / 2
        } else {
            b.right
        };
        let (min_key, max_key) = self.setup_tab_search(right, mid_y);
        let mut it = self.v_it;
        while !it.at_first(l) && self.key(it.data(l)) >= min_key {
            it.backward(l);
        }
        while !it.at_last(l) && self.key(it.data(l)) < min_key {
            it.forward(l);
        }
        let mut best: Option<VecId> = None;
        let mut best_x = -1;
        let mut key_limit = -1;
        loop {
            let vid = it.data(l);
            let v = self.tv.get(vid);
            let x = v.x_at_y(mid_y);
            if x >= right
                && (v.v_overlap_y(top_y, bottom_y) > 0
                    || (extended && v.extended_overlap(top_y, bottom_y) > 0))
                && (best.is_none() || x < best_x)
            {
                best = Some(vid);
                best_x = x;
                key_limit = v.sort_key + max_key - min_key;
            }
            if it.at_last(l) || (best.is_some() && v.sort_key > key_limit) {
                break;
            }
            it.forward(l);
            if it.at_first(l) {
                break;
            }
        }
        self.v_it = it;
        best
    }

    pub fn left_tab_for_box(&mut self, b: &TBox, crossing: bool, extended: bool) -> Option<VecId> {
        let l = &self.vectors;
        if l.is_empty() {
            return None;
        }
        let (top_y, bottom_y) = (b.top, b.bottom);
        let mid_y = (top_y + bottom_y) / 2;
        let left = if crossing {
            (b.left + b.right) / 2
        } else {
            b.left
        };
        let (min_key, max_key) = self.setup_tab_search(left, mid_y);
        let mut it = self.v_it;
        while !it.at_last(l) && self.key(it.data(l)) <= max_key {
            it.forward(l);
        }
        while !it.at_first(l) && self.key(it.data(l)) > max_key {
            it.backward(l);
        }
        let mut best: Option<VecId> = None;
        let mut best_x = -1;
        let mut key_limit = -1;
        loop {
            let vid = it.data(l);
            let v = self.tv.get(vid);
            let x = v.x_at_y(mid_y);
            if x <= left
                && (v.v_overlap_y(top_y, bottom_y) > 0
                    || (extended && v.extended_overlap(top_y, bottom_y) > 0))
                && (best.is_none() || x > best_x)
            {
                best = Some(vid);
                best_x = x;
                key_limit = v.sort_key - (max_key - min_key);
            }
            if it.at_first(l) || (best.is_some() && v.sort_key < key_limit) {
                break;
            }
            it.backward(l);
            if it.at_last(l) {
                break;
            }
        }
        self.v_it = it;
        best
    }

    pub fn different_sizes(a: i32, b: i32) -> bool {
        a > b * 2 || b > a * 2
    }

    pub fn very_different_sizes(a: i32, b: i32) -> bool {
        a > b * 5 || b > a * 5
    }

    /// `TidyBlobs`.
    pub fn tidy_blobs(&mut self, blobs: &mut Blobs, tb: &mut ToBlock) {
        let mut large_it = Iter::new(&tb.large_blobs);
        let mut blob_it = Iter::new(&tb.blobs);
        large_it.mark_cycle_pt();
        while !large_it.cycled_list(&tb.large_blobs) {
            let b = large_it.data(&tb.large_blobs);
            if blobs.get(b).owner.is_some() {
                let v = large_it.extract(&mut tb.large_blobs);
                blob_it.add_to_end(&mut tb.blobs, v);
            }
            large_it.forward(&tb.large_blobs);
        }
        tb.delete_unowned_noise(blobs);
    }

    fn setup_tab_search(&self, x: i32, y: i32) -> (i32, i32) {
        let key1 = TabVector::sort_key_of(self.vertical_skew, x, (y + self.tright().y) / 2);
        let key2 = TabVector::sort_key_of(self.vertical_skew, x, (y + self.bleft().y) / 2);
        (key1.min(key2), key1.max(key2))
    }

    /// `FindInitialTabVectors` (no image blobs).
    pub fn find_initial_tab_vectors(
        &mut self,
        blobs: &mut Blobs,
        min_gutter_width: i32,
        aligned_gap_fraction: f64,
        tb: &ToBlock,
    ) {
        self.insert_blobs_to_grid(blobs, true, false, &tb.blobs);
        self.find_tab_boxes(blobs, &tb.blobs, min_gutter_width, aligned_gap_fraction);
        self.find_all_tab_vectors(blobs, min_gutter_width);
        let vs = self.vertical_skew;
        let mut vectors = std::mem::take(&mut self.vectors);
        self.tv
            .merge_similar_tab_vectors(blobs, vs, &mut vectors, Some(&self.grid));
        self.vectors = vectors;
        self.sort_vectors();
        self.evaluate_tabs(blobs);
        self.mark_vertical_text(blobs);
    }

    /// `FindTabBoxes`. `owner_list` is the list holding the grid's blobs:
    /// Tesseract 5.3.4 sorts with a comparator that dereferences the
    /// blob's embedded list link, so each blob is ordered by the box of its
    /// successor in that list.
    fn find_tab_boxes(
        &mut self,
        blobs: &mut Blobs,
        owner_list: &EList<BlobId>,
        min_gutter_width: i32,
        aligned_gap_fraction: f64,
    ) {
        self.left_tab_boxes.clear();
        self.right_tab_boxes.clear();
        let mut s = GridSearch::new();
        s.start_full_search(&self.grid);
        while let Some(b) = s.next_full_search(&self.grid, &*blobs) {
            if self.test_box_for_tabs(blobs, b, min_gutter_width, aligned_gap_fraction) {
                if blobs.get(b).left_tab_type != TabType::None {
                    self.left_tab_boxes.push(b);
                }
                if blobs.get(b).right_tab_type != TabType::None {
                    self.right_tab_boxes.push(b);
                }
            }
        }
        let order = owner_list.to_vec();
        let mut succ: HashMap<BlobId, BlobId> = HashMap::with_capacity(order.len());
        for (i, &b) in order.iter().enumerate() {
            succ.insert(b, order[(i + 1) % order.len()]);
        }
        let key = |b: &BlobId| blobs.get(*succ.get(b).copied().as_ref().unwrap_or(b)).bbox;
        stdalgo::sort(&mut self.left_tab_boxes, |a, b| {
            super::grid::sort_by_box_left(&key(a), &key(b)) < 0
        });
        stdalgo::sort(&mut self.right_tab_boxes, |a, b| {
            super::grid::sort_right_to_left(&key(a), &key(b)) < 0
        });
    }

    fn test_box_for_tabs(
        &mut self,
        blobs: &mut Blobs,
        bbox: BlobId,
        min_gutter_width: i32,
        aligned_gap_fraction: f64,
    ) -> bool {
        let bb = blobs.get(bbox).clone();
        let bx = bb.bbox;
        let left_column_edge = bb.left_rule;
        let right_column_edge = bb.right_rule;
        let (left_x, right_x, top_y, bottom_y) = (bx.left, bx.right, bx.top, bx.bottom);
        let height = bx.height();
        let gs = self.gridsize();
        let radius = (height * TAB_RADIUS_FACTOR + gs - 1) / gs;
        let mut rs = GridSearch::new();
        rs.start_rad_search(
            &self.grid,
            (left_x + right_x) / 2,
            (top_y + bottom_y) / 2,
            radius,
        );
        let mut min_spacing = (f64::from(height) * aligned_gap_fraction) as i32;
        if min_gutter_width > min_spacing {
            min_spacing = min_gutter_width;
        }
        let mut min_ragged_gutter = RAGGED_GUTTER_MULTIPLE * gs;
        if min_gutter_width > min_ragged_gutter {
            min_ragged_gutter = min_gutter_width;
        }
        let target_right = left_x - min_spacing;
        let target_left = right_x + min_spacing;
        let mut is_left_tab = true;
        let mut is_right_tab = true;
        let mut maybe_ragged_left = true;
        let mut maybe_ragged_right = true;
        let (mut ml_up, mut mr_up, mut ml_down, mut mr_down) = (0i32, 0i32, 0i32, 0i32);
        const NEG: i32 = -i32::MAX;
        if bb.leader_on_left {
            is_left_tab = false;
            maybe_ragged_left = false;
            ml_up = NEG;
            ml_down = NEG;
        }
        if bb.leader_on_right {
            is_right_tab = false;
            maybe_ragged_right = false;
            mr_up = NEG;
            mr_down = NEG;
        }
        let alignment_tolerance = (f64::from(self.resolution) * ALIGNED_FRACTION) as i32;
        while let Some(n) = rs.next_rad_search(&self.grid) {
            if n == bbox {
                continue;
            }
            let nb = blobs.get(n);
            let nbox = nb.bbox;
            let (n_left, n_right) = (nbox.left, nbox.right);
            if n_right > right_column_edge
                || n_left < left_column_edge
                || left_x < nb.left_rule
                || right_x > nb.right_rule
            {
                continue;
            }
            let n_mid_x = (n_left + n_right) / 2;
            let n_mid_y = (nbox.top + nbox.bottom) / 2;
            if n_mid_x <= left_x && n_right >= target_right {
                is_left_tab = false;
                if n_mid_y < top_y {
                    ml_down = NEG;
                }
                if n_mid_y > bottom_y {
                    ml_up = NEG;
                }
            } else if nearly_equal(left_x, n_left, alignment_tolerance) {
                if n_mid_y > top_y && ml_up > NEG {
                    ml_up += 1;
                }
                if n_mid_y < bottom_y && ml_down > NEG {
                    ml_down += 1;
                }
            } else if n_left < left_x && n_right >= left_x {
                if n_mid_y > top_y && ml_up > NEG {
                    ml_up -= 1;
                }
                if n_mid_y < bottom_y && ml_down > NEG {
                    ml_down -= 1;
                }
            }
            if n_left < left_x && nbox.y_overlap(&bx) && n_right >= target_right {
                maybe_ragged_left = false;
            }
            if n_mid_x >= right_x && n_left <= target_left {
                is_right_tab = false;
                if n_mid_y < top_y {
                    mr_down = NEG;
                }
                if n_mid_y > bottom_y {
                    mr_up = NEG;
                }
            } else if nearly_equal(right_x, n_right, alignment_tolerance) {
                if n_mid_y > top_y && mr_up > NEG {
                    mr_up += 1;
                }
                if n_mid_y < bottom_y && mr_down > NEG {
                    mr_down += 1;
                }
            } else if n_right > right_x && n_left <= right_x {
                if n_mid_y > top_y && mr_up > NEG {
                    mr_up -= 1;
                }
                if n_mid_y < bottom_y && mr_down > NEG {
                    mr_down -= 1;
                }
            }
            if n_right > right_x && nbox.y_overlap(&bx) && n_left <= target_left {
                maybe_ragged_right = false;
            }
            if ml_down == NEG && ml_up == NEG && mr_down == NEG && mr_up == NEG {
                break;
            }
        }
        let lt = if is_left_tab || ml_up > 1 || ml_down > 1 {
            TabType::MaybeAligned
        } else if maybe_ragged_left && self.confirm_ragged(blobs, bbox, min_ragged_gutter, true) {
            TabType::MaybeRagged
        } else {
            TabType::None
        };
        blobs.get_mut(bbox).left_tab_type = lt;
        let rt = if is_right_tab || mr_up > 1 || mr_down > 1 {
            TabType::MaybeAligned
        } else if maybe_ragged_right && self.confirm_ragged(blobs, bbox, min_ragged_gutter, false) {
            TabType::MaybeRagged
        } else {
            TabType::None
        };
        blobs.get_mut(bbox).right_tab_type = rt;
        lt != TabType::None || rt != TabType::None
    }

    fn confirm_ragged(&self, blobs: &Blobs, bbox: BlobId, min_gutter: i32, left: bool) -> bool {
        let bx = blobs.get(bbox).bbox;
        let mut sb = bx;
        if left {
            sb.right = sb.left;
            sb.left -= min_gutter;
        } else {
            sb.left = sb.right;
            sb.right += min_gutter;
        }
        self.nothing_y_overlaps_in_box(blobs, &sb, &bx)
    }

    fn nothing_y_overlaps_in_box(&self, blobs: &Blobs, search: &TBox, target: &TBox) -> bool {
        let mut s = GridSearch::new();
        s.start_rect_search(&self.grid, search);
        while let Some(b) = s.next_rect_search(&self.grid, blobs) {
            let bx = blobs.get(b).bbox;
            if bx.y_overlap(target) && bx != *target {
                return false;
            }
        }
        true
    }

    fn find_all_tab_vectors(&mut self, blobs: &mut Blobs, min_gutter_width: i32) {
        let mut dummy: EList<VecId> = EList::new();
        let mut vertical_x = 0;
        let mut vertical_y = 1;
        let mut search_size = MIN_VERTICAL_SEARCH;
        while search_size < MAX_VERTICAL_SEARCH {
            let mut count = self.find_tab_vectors(
                blobs,
                search_size,
                TabAlignment::LeftAligned,
                min_gutter_width,
                &mut dummy,
                &mut vertical_x,
                &mut vertical_y,
            );
            count += self.find_tab_vectors(
                blobs,
                search_size,
                TabAlignment::RightAligned,
                min_gutter_width,
                &mut dummy,
                &mut vertical_x,
                &mut vertical_y,
            );
            if count > 0 {
                break;
            }
            search_size += MIN_VERTICAL_SEARCH;
        }
        dummy = EList::new();
        for &b in &self.left_tab_boxes {
            if blobs.get(b).left_tab_type == TabType::Confirmed {
                blobs.get_mut(b).left_tab_type = TabType::MaybeAligned;
            }
        }
        for &b in &self.right_tab_boxes {
            if blobs.get(b).right_tab_type == TabType::Confirmed {
                blobs.get_mut(b).right_tab_type = TabType::MaybeAligned;
            }
        }
        for (size, align) in [
            (MAX_VERTICAL_SEARCH, TabAlignment::LeftAligned),
            (MAX_VERTICAL_SEARCH, TabAlignment::RightAligned),
            (MAX_RAGGED_SEARCH, TabAlignment::LeftRagged),
            (MAX_RAGGED_SEARCH, TabAlignment::RightRagged),
        ] {
            self.find_tab_vectors(
                blobs,
                size,
                align,
                min_gutter_width,
                &mut dummy,
                &mut vertical_x,
                &mut vertical_y,
            );
        }
        let mut it = Iter::new(&self.vectors);
        it.add_list_after(&mut self.vectors, &dummy.to_vec());
        self.set_vertical_skew_and_parallelize(blobs, vertical_x, vertical_y);
    }

    #[allow(clippy::too_many_arguments)]
    fn find_tab_vectors(
        &mut self,
        blobs: &mut Blobs,
        search_size_multiple: i32,
        alignment: TabAlignment,
        min_gutter_width: i32,
        vectors: &mut EList<VecId>,
        vertical_x: &mut i32,
        vertical_y: &mut i32,
    ) -> i32 {
        let mut it = Iter::new(vectors);
        let mut count = 0;
        let right = matches!(
            alignment,
            TabAlignment::RightAligned | TabAlignment::RightRagged
        );
        let boxes = if right {
            self.right_tab_boxes.clone()
        } else {
            self.left_tab_boxes.clone()
        };
        for b in boxes {
            let bb = blobs.get(b);
            if (!right && bb.left_tab_type == TabType::MaybeAligned)
                || (right && bb.right_tab_type == TabType::MaybeAligned)
            {
                let height = bb.bbox.height().max(self.gridsize());
                let p = AlignedBlobParams::new(
                    *vertical_x,
                    *vertical_y,
                    height,
                    search_size_multiple,
                    min_gutter_width,
                    self.resolution,
                    alignment,
                );
                if let Some(v) = find_vertical_alignment(
                    &self.grid,
                    blobs,
                    &mut self.tv,
                    &p,
                    b,
                    vertical_x,
                    vertical_y,
                ) {
                    count += 1;
                    it.add_to_end(vectors, v);
                }
            }
        }
        count
    }

    pub fn set_vertical_skew_and_parallelize(
        &mut self,
        blobs: &Blobs,
        vertical_x: i32,
        vertical_y: i32,
    ) {
        self.vertical_skew = shrink(vertical_x, vertical_y);
        let vs = self.vertical_skew;
        for v in self.vectors.to_vec() {
            self.tv.get_mut(v).fit(blobs, vs, true);
        }
        self.sort_vectors();
    }

    pub fn sort_vectors(&mut self) {
        let tv = &self.tv;
        self.vectors
            .sort_by(|a, b| tv.get(*a).sort_key.cmp(&tv.get(*b).sort_key));
        self.v_it = Iter::new(&self.vectors);
    }

    fn evaluate_tabs(&mut self, blobs: &Blobs) {
        let mut it = Iter::new(&self.vectors);
        it.mark_cycle_pt();
        while !it.cycled_list(&self.vectors) {
            let v = it.data(&self.vectors);
            if !self.tv.get(v).is_separator() {
                self.evaluate(blobs, v);
                if self.tv.get(v).box_count() < MIN_EVALUATED_TABS {
                    it.extract(&mut self.vectors);
                    self.v_it = Iter::new(&self.vectors);
                }
            }
            it.forward(&self.vectors);
        }
    }

    fn mark_vertical_text(&mut self, blobs: &mut Blobs) {
        let mut s = GridSearch::new();
        s.start_full_search(&self.grid);
        while let Some(b) = s.next_full_search(&self.grid, &*blobs) {
            let bb = blobs.get_mut(b);
            if bb.region_type < RegionType::Unknown {
                continue;
            }
            if bb.vert_possible && !bb.horz_possible {
                bb.region_type = RegionType::VertText;
            }
        }
    }

    /// `AdjacentBlob`.
    #[allow(clippy::too_many_arguments)]
    pub fn adjacent_blob(
        &self,
        blobs: &Blobs,
        bbox: BlobId,
        look_left: bool,
        ignore_images: bool,
        min_overlap_fraction: f64,
        gap_limit: i32,
        top_y: i32,
        bottom_y: i32,
    ) -> Option<BlobId> {
        let bx = blobs.get(bbox).bbox;
        let (left, right) = (bx.left, bx.right);
        let mid_x = (left + right) / 2;
        let mut s = GridSearch::new();
        s.start_side_search(&self.grid, mid_x, bottom_y, top_y);
        let mut best_gap = 0;
        let mut result = None;
        while let Some(n) = s.next_side_search(&self.grid, look_left) {
            let nb = blobs.get(n);
            if n == bbox || (ignore_images && nb.region_type < RegionType::Unknown) {
                continue;
            }
            let nbox = nb.bbox;
            let v_overlap = nbox.top.min(top_y) - nbox.bottom.max(bottom_y);
            let height = top_y - bottom_y;
            let n_height = nbox.top - nbox.bottom;
            if f64::from(v_overlap) > min_overlap_fraction * f64::from(height.min(n_height))
                && (min_overlap_fraction == 0.0 || !TabFind::different_sizes(height, n_height))
            {
                let (n_left, n_right) = (nbox.left, nbox.right);
                let h_gap = n_left.max(left) - n_right.min(right);
                let n_mid_x = (n_left + n_right) / 2;
                if look_left == (n_mid_x < mid_x) && n_mid_x != mid_x {
                    if h_gap > gap_limit {
                        return result;
                    }
                    let t = if look_left {
                        nb.right_tab_type
                    } else {
                        nb.left_tab_type
                    };
                    if h_gap > 0 && t >= TabType::Confirmed {
                        return result;
                    }
                    if result.is_none() || h_gap < best_gap {
                        result = Some(n);
                        best_gap = h_gap;
                    } else {
                        return result;
                    }
                }
            }
        }
        result
    }

    /// `TabFind::Reset`: drops non-separator vectors and clears the grid.
    pub fn reset(&mut self) {
        let mut it = self.v_it;
        it.move_to_first(&self.vectors);
        it.mark_cycle_pt();
        while !it.cycled_list(&self.vectors) {
            let v = it.data(&self.vectors);
            if !self.tv.get(v).is_separator() {
                it.extract(&mut self.vectors);
            }
            it.forward(&self.vectors);
        }
        self.v_it = it;
        self.grid.clear();
    }

    // ---- TabVector evaluation (needs live access to all vectors) ----

    fn fit_and_evaluate_if_needed(&mut self, blobs: &Blobs, id: VecId) {
        let vs = self.vertical_skew;
        if self.tv.get(id).needs_refit {
            self.tv.get_mut(id).fit(blobs, vs, true);
        }
        if self.tv.get(id).needs_evaluation {
            self.evaluate(blobs, id);
        }
    }

    /// `TabVector::Evaluate`.
    fn evaluate(&mut self, blobs: &Blobs, id: VecId) {
        self.tv.get_mut(id).needs_evaluation = false;
        let length = self.tv.get(id).endpt.y - self.tv.get(id).startpt.y;
        if length == 0 || self.tv.get(id).boxes.is_empty() {
            self.tv.get_mut(id).percent_score = 0;
            return;
        }
        let ids = self.tv.get(id).boxes.to_vec();
        let mut mean_height = 0;
        for &b in &ids {
            mean_height += blobs.get(b).bbox.height();
        }
        mean_height /= ids.len() as i32;
        let ragged = self.tv.get(id).is_ragged();
        let left = self.tv.get(id).is_left_tab();
        let max_gutter = if ragged {
            GUTTER_TO_NEIGHBOUR_RATIO * mean_height
        } else {
            GUTTER_MULTIPLE * mean_height
        };
        let mut gutters = Stats::new(0, max_gutter);
        let mut num_deleted = 0;
        let mut text_on_image = false;
        let mut good_length = 0;
        let mut prev_good: Option<TBox> = None;
        let mut it = Iter::new(&self.tv.get(id).boxes);
        it.mark_cycle_pt();
        while !it.cycled_list(&self.tv.get(id).boxes) {
            let bbox = it.data(&self.tv.get(id).boxes);
            let b = blobs.get(bbox).bbox;
            let mid_y = (b.top + b.bottom) / 2;
            let tab_x = self.tv.get(id).x_at_y(mid_y);
            let (gutter_width, neighbour_gap) = self.gutter_width_and_neighbour_gap(
                blobs,
                tab_x,
                mean_height,
                max_gutter,
                left,
                bbox,
            );
            if neighbour_gap * GUTTER_TO_NEIGHBOUR_RATIO <= gutter_width {
                good_length += b.top - b.bottom;
                gutters.add(gutter_width, 1);
                if let Some(p) = prev_good {
                    let vertical_gap = b.bottom - p.top;
                    let size1 = f64::from(p.area()).sqrt();
                    let size2 = f64::from(b.area()).sqrt();
                    if f64::from(vertical_gap) < MAX_FILLIN_MULTIPLE * size1.min(size2) {
                        good_length += vertical_gap;
                    }
                } else {
                    self.tv.get_mut(id).set_y_start(b.bottom);
                }
                prev_good = Some(b);
                if blobs.get(bbox).flow == FlowType::TextOnImage {
                    text_on_image = true;
                }
            } else {
                it.extract(&mut self.tv.get_mut(id).boxes);
                num_deleted += 1;
            }
            it.forward(&self.tv.get(id).boxes);
        }
        let mut search_top = self.tv.get(id).endpt.y;
        let mut search_bottom = self.tv.get(id).startpt.y;
        let median_gutter = int_cast_rounded(gutters.median());
        if gutters.get_total() > 0 {
            prev_good = None;
            let mut it = Iter::new(&self.tv.get(id).boxes);
            it.mark_cycle_pt();
            while !it.cycled_list(&self.tv.get(id).boxes) {
                let bbox = it.data(&self.tv.get(id).boxes);
                let b = blobs.get(bbox).bbox;
                let mid_y = (b.top + b.bottom) / 2;
                let tab_x = self.tv.get(id).x_at_y(mid_y);
                let (gutter_width, _) = self.gutter_width_and_neighbour_gap(
                    blobs,
                    tab_x,
                    mean_height,
                    max_gutter,
                    left,
                    bbox,
                );
                if f64::from(gutter_width) >= f64::from(median_gutter) * MIN_GUTTER_FRACTION {
                    if prev_good.is_none() {
                        self.tv.get_mut(id).set_y_start(b.bottom);
                        search_bottom = b.top;
                    }
                    prev_good = Some(b);
                    search_top = b.bottom;
                } else {
                    it.extract(&mut self.tv.get_mut(id).boxes);
                    num_deleted += 1;
                }
                it.forward(&self.tv.get(id).boxes);
            }
        }
        let Some(p) = prev_good else {
            self.tv.get_mut(id).percent_score = 0;
            return;
        };
        {
            let v = self.tv.get_mut(id);
            v.set_y_end(p.top);
            let length = v.endpt.y - v.startpt.y;
            v.percent_score = 100 * good_length / length;
        }
        if num_deleted > 0 {
            self.tv.get_mut(id).needs_refit = true;
            self.fit_and_evaluate_if_needed(blobs, id);
            if self.tv.get(id).boxes.is_empty() {
                return;
            }
        }
        if search_bottom > search_top {
            search_bottom = self.tv.get(id).startpt.y;
            search_top = self.tv.get(id).endpt.y;
        }
        let v = self.tv.get(id);
        let mut min_gutter_width = LINE_COUNT_RECIPROCAL / v.boxes.len() as f64;
        min_gutter_width += if v.is_ragged() {
            MIN_RAGGED_GUTTER
        } else {
            MIN_ALIGNED_GUTTER
        };
        min_gutter_width *= f64::from(mean_height);
        let mut max_gutter_width = int_cast_rounded(min_gutter_width) + 1;
        if median_gutter > max_gutter_width {
            max_gutter_width = median_gutter;
        }
        let (gutter_width, _) = self.gutter_width(
            blobs,
            search_bottom,
            search_top,
            v,
            text_on_image,
            max_gutter_width,
        );
        if f64::from(gutter_width) < min_gutter_width {
            let v = self.tv.get_mut(id);
            v.boxes = EList::new();
            v.percent_score = 0;
        }
    }
}

pub fn unmergeable(t: RegionType) -> bool {
    matches!(
        t,
        RegionType::HLine | RegionType::VLine | RegionType::RectImage | RegionType::PolyImage
    )
}
