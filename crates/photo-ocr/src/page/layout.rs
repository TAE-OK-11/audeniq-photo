//! The sparse-text (`--psm 11`) page layout: `ColumnFinder` setup,
//! `StrokeWidth` neighbour analysis, `CCNonTextDetect` and partition
//! building, ending in one block per textline partition
//! (`colfind.cpp`, `strokewidth.cpp`, `ccnontextdetect.cpp`).

use super::bitmap::Bitmap;
use super::blobbox::{
    BND_ABOVE, BND_BELOW, BND_LEFT, BND_RIGHT, BlobId, Blobs, FlowType, RegionType, ToBlock,
};
use super::colpartition::{ColPartition, PartId, Parts, PolyBlockType, is_text_type, unmergeable};
use super::detlinefit::int_cast_rounded;
use super::elist::{EList, Iter};
use super::geom::{ICoord, TBox};
use super::grid::{BBGrid, GridSearch, IntGrid, clist_add_sorted, sort_by_box_left};
use super::lept::{box_create, clip_box_to_foreground};
use super::morph::LBox;
use super::partgrid::{PartGrid, PartGridOps, blank_image_in_between, list_2nd_neighbours};
use super::projection::TextlineProjection;
use super::stats::Stats;
use super::tabfind::TabFind;

const STROKE_WIDTH_FRACTION_TOLERANCE: f64 = 0.125;
const STROKE_WIDTH_TOLERANCE: f64 = 1.5;
const DIACRITIC_X_PAD_RATIO: f64 = 7.0;
const DIACRITIC_Y_PAD_RATIO: f64 = 1.75;
const MIN_DIACRITIC_SIZE_RATIO: f64 = 1.0625;
const MAX_DIACRITIC_DISTANCE_RATIO: f64 = 1.25;
const MAX_DIACRITIC_GAP_TO_BASE_CHAR_HEIGHT: f64 = 1.0;
const LINE_TRAP_LONGEST: i32 = 4;
const LINE_TRAP_SHORTEST: i32 = 2;
const LINE_RESIDUE_ASPECT_RATIO: f64 = 8.0;
const LINE_RESIDUE_PAD_RATIO: i32 = 3;
const LINE_RESIDUE_SIZE_RATIO: f64 = 1.75;
const NEIGHBOUR_SEARCH_FACTOR: f64 = 2.5;
const NOISE_OVERLAP_GROWTH_FACTOR: f64 = 4.0;
const NOISE_OVERLAP_AREA_FACTOR: f64 = 1.0 / 512.0;

const MIN_GUTTER_WIDTH_GRID: f64 = 0.5;
const ALIGNED_GAP_FRACTION: f64 = 0.75;

const MAX_SMALL_NEIGHBOURS_PER_PIX: f64 = 1.0 / 32.0;
const MAX_LARGE_OVERLAPS_WITH_SMALL: i32 = 3;
const MAX_MEDIUM_OVERLAPS_WITH_SMALL: i32 = 12;
const MAX_LARGE_OVERLAPS_WITH_MEDIUM: i32 = 12;
const ORIGINAL_NOISE_MULTIPLE: i32 = 8;
const NOISE_PADDING: i32 = 4;
const PHOTO_OFFSET_FRACTION: f64 = 0.375;
const MIN_GOOD_TEXT_PA_RATIO: f64 = 1.5;

/// A layout block: one textline partition (`BLOCK` + `TO_BLOCK` + its
/// single `TO_ROW`).
#[derive(Debug, Clone)]
pub struct LayoutBlock {
    pub bbox: TBox,
    pub ptype: PolyBlockType,
    pub index: i32,
    pub line_size: f32,
    pub line_spacing: f32,
    pub max_blob_size: f32,
    pub median_size: ICoord,
    pub row: Row,
}

/// `TO_ROW` as produced by `ColPartition::MakeToRow`.
#[derive(Debug, Clone)]
pub struct Row {
    pub blobs: EList<BlobId>,
    pub y_min: f32,
    pub y_max: f32,
    pub initial_y_min: f32,
}

impl Row {
    fn new(blob: BlobId, top: f32, bottom: f32, row_size: f32) -> Row {
        let mut r = Row {
            blobs: EList::new(),
            y_min: bottom,
            y_max: top,
            initial_y_min: bottom,
        };
        r.blobs.push_back(blob);
        let diff = top - bottom - row_size;
        if diff > 0.0 {
            r.y_max -= diff / 2.0;
            r.y_min += diff / 2.0;
        } else if (top - bottom) * 3.0 < row_size {
            let diff = row_size / 3.0 + bottom - top;
            r.y_max += diff / 2.0;
            r.y_min -= diff / 2.0;
        }
        r
    }

    fn add_blob(&mut self, blob: BlobId, top: f32, bottom: f32, row_size: f32) {
        let mut it = Iter::new(&self.blobs);
        it.add_to_end(&mut self.blobs, blob);
        let allowed = row_size + self.y_min - self.y_max;
        if allowed > 0.0 {
            let mut available = if top > self.y_max {
                top - self.y_max
            } else {
                0.0
            };
            if bottom < self.y_min {
                available += self.y_min - bottom;
            }
            if available > 0.0 {
                available += available;
                if available < allowed {
                    available = allowed;
                }
                if bottom < self.y_min {
                    self.y_min -= (self.y_min - bottom) * allowed / available;
                }
                if top > self.y_max {
                    self.y_max += (top - self.y_max) * allowed / available;
                }
            }
        }
    }
}

pub struct Layout {
    pub tf: TabFind,
    pub sw: BBGrid<BlobId>,
    pub part_grid: PartGrid,
    pub parts: Parts,
    pub projection: TextlineProjection,
    pub nontext_map: Bitmap,
    pub big_parts: EList<PartId>,
    pub image_bblobs: EList<BlobId>,
    pub min_gutter_width: i32,
    grid_box: TBox,
}

impl Layout {
    /// `ColumnFinder::ColumnFinder` (no vertical lines in sparse mode).
    pub fn new(
        gridsize: i32,
        bleft: ICoord,
        tright: ICoord,
        resolution: i32,
        vertical_x: i32,
        vertical_y: i32,
    ) -> Layout {
        Layout {
            tf: TabFind::new(gridsize, bleft, tright, vertical_x, vertical_y, resolution),
            sw: BBGrid::new(gridsize, bleft, tright),
            part_grid: BBGrid::new(gridsize, bleft, tright),
            parts: Parts::default(),
            projection: TextlineProjection::new(resolution),
            nontext_map: Bitmap::new(0, 0),
            big_parts: EList::new(),
            image_bblobs: EList::new(),
            min_gutter_width: (MIN_GUTTER_WIDTH_GRID * f64::from(gridsize)) as i32,
            grid_box: TBox::new(bleft.x, bleft.y, tright.x, tright.y),
        }
    }

    fn gridsize(&self) -> i32 {
        self.tf.gridsize()
    }

    fn insert_blob_list(grid: &mut BBGrid<BlobId>, blobs: &Blobs, list: &EList<BlobId>) {
        for b in list.to_vec() {
            if !blobs.get(b).joined {
                grid.insert_bbox(blobs, true, true, b);
            }
        }
    }

    fn pg<'a>(&'a mut self, blobs: &'a mut Blobs) -> PartGridOps<'a> {
        PartGridOps {
            grid: &mut self.part_grid,
            parts: &mut self.parts,
            blobs,
        }
    }

    /// `ColumnFinder::SetupAndFilterNoise`.
    pub fn setup_and_filter_noise(
        &mut self,
        blobs: &mut Blobs,
        photo_mask: &Bitmap,
        tb: &mut ToBlock,
    ) {
        let (gs, bl, tr) = (self.gridsize(), self.tf.bleft(), self.tf.tright());
        self.part_grid = BBGrid::new(gs, bl, tr);
        self.sw = BBGrid::new(gs, bl, tr);
        self.min_gutter_width = (MIN_GUTTER_WIDTH_GRID * f64::from(gs)) as i32;
        tb.re_set_and_re_filter_blobs(blobs);
        self.tf.set_block_rule_edges(blobs, tb);
        // SetNeighboursOnMediumBlobs
        Self::insert_blob_list(&mut self.sw, blobs, &tb.blobs);
        for b in tb.blobs.to_vec() {
            self.set_neighbours(blobs, false, false, b);
        }
        self.sw.clear();
        self.nontext_map = compute_non_text_mask(gs, bl, tr, blobs, photo_mask, tb);
        // FindTextlineDirectionAndFixBrokenCJK (no CJK merging)
        Self::insert_blob_list(&mut self.sw, blobs, &tb.blobs);
        Self::insert_blob_list(&mut self.sw, blobs, &tb.large_blobs);
        self.find_textline_flow_direction(blobs);
        self.sw.clear();
    }

    /// `ColumnFinder::FindBlocks` for the sparse modes. Returns the blocks
    /// and the diacritic blobs set aside as noise.
    pub fn find_blocks(
        &mut self,
        blobs: &mut Blobs,
        tb: &mut ToBlock,
    ) -> (Vec<LayoutBlock>, EList<BlobId>) {
        let mut diacritic_blobs: EList<BlobId> = EList::new();
        self.find_leader_partitions(blobs, tb);
        self.remove_line_residue(blobs);
        self.tf
            .find_initial_tab_vectors(blobs, self.min_gutter_width, ALIGNED_GAP_FRACTION, tb);
        self.tf.set_block_rule_edges(blobs, tb);
        self.grade_blobs_into_partitions(blobs, tb, &mut diacritic_blobs);
        let mut im = std::mem::take(&mut self.image_bblobs);
        self.pg(blobs).re_type_blobs(&mut im);
        self.image_bblobs = im;
        self.tf.tidy_blobs(blobs, tb);
        self.tf.reset();
        for p in self.big_parts.take_all() {
            self.parts.disown_boxes_no_assert(blobs, p);
            self.parts.delete(p);
        }
        // ReleaseBlobsAndCleanupUnused
        for list in [
            &mut tb.blobs,
            &mut tb.small_blobs,
            &mut tb.noise_blobs,
            &mut tb.large_blobs,
            &mut self.image_bblobs,
        ] {
            for b in list.take_all() {
                if blobs.get(b).owner.is_none() {
                    blobs.delete(b);
                }
            }
        }
        let mut out = self.extract_partitions_as_blocks(blobs);
        rotate_and_reskew_blocks(blobs, &mut out);
        (out, diacritic_blobs)
    }

    /// `ColPartitionGrid::ExtractPartitionsAsBlocks`.
    fn extract_partitions_as_blocks(&mut self, blobs: &mut Blobs) -> Vec<LayoutBlock> {
        let mut out = Vec::new();
        let mut gs = GridSearch::new();
        gs.start_full_search(&self.part_grid);
        while let Some(part) = gs.next_full_search(&self.part_grid, &self.parts) {
            let p = self.parts.get(part);
            let blob_type = p.blob_type;
            if is_text_type(blob_type) || (blob_type == RegionType::Unknown && p.boxes_count() > 1)
            {
                let ptype = if blob_type == RegionType::VertText {
                    PolyBlockType::VerticalText
                } else {
                    PolyBlockType::FlowingText
                };
                let b = p.bounding_box;
                let (median_width, median_height) = (p.median_width, p.median_height);
                let Some(row) = self.make_to_row(blobs, part) else {
                    self.parts.delete_boxes(blobs, part);
                    continue;
                };
                let (mut line_size, line_spacing, max_blob_size) =
                    if blob_type == RegionType::VertText {
                        (
                            median_width as f32,
                            b.width() as f32,
                            (b.width() + 1) as f32,
                        )
                    } else {
                        (
                            median_height as f32,
                            b.height() as f32,
                            (b.height() + 1) as f32,
                        )
                    };
                if line_size == 0.0 {
                    line_size = 1.0;
                }
                out.push(LayoutBlock {
                    bbox: b,
                    ptype,
                    index: 0,
                    line_size,
                    line_spacing,
                    max_blob_size,
                    median_size: ICoord::default(),
                    row,
                });
            } else {
                self.parts.delete_boxes(blobs, part);
            }
        }
        self.part_grid.clear();
        out
    }

    /// `ColPartition::MakeToRow`.
    fn make_to_row(&mut self, blobs: &Blobs, part: PartId) -> Option<Row> {
        let p = self.parts.get_mut(part);
        let line_size = if p.is_vertical_type() {
            p.median_width
        } else {
            p.median_height
        } as f32;
        let mut row: Option<Row> = None;
        for b in p.boxes.take_all() {
            let bx = blobs.get(b).bbox;
            let (top, bottom) = (bx.top as f32, bx.bottom as f32);
            match row.as_mut() {
                None => row = Some(Row::new(b, top, bottom, line_size)),
                Some(r) => r.add_blob(b, top, bottom, line_size),
            }
        }
        row
    }

    // ---------------------------------------------------------------------
    // StrokeWidth.

    /// `SetNeighbours`.
    fn set_neighbours(
        &mut self,
        blobs: &mut Blobs,
        leaders: bool,
        activate_line_trap: bool,
        blob: BlobId,
    ) {
        let mut line_trap_count = 0;
        for dir in 0..4 {
            line_trap_count += self.find_good_neighbour(blobs, dir, leaders, blob);
        }
        if line_trap_count > 0 && activate_line_trap {
            let b = blobs.get_mut(blob);
            b.clear_neighbours();
            b.region_type = if b.bbox.width() > b.bbox.height() {
                RegionType::HLine
            } else {
                RegionType::VLine
            };
        }
    }

    /// `FindGoodNeighbour`.
    fn find_good_neighbour(
        &mut self,
        blobs: &mut Blobs,
        dir: usize,
        leaders: bool,
        blob: BlobId,
    ) -> i32 {
        let bb = blobs.get(blob).clone();
        let b = bb.bbox;
        let (top, bottom, left, right) = (b.top, b.bottom, b.left, b.right);
        let width = right - left;
        let height = top - bottom;
        let line_trap_max = width.max(height) / LINE_TRAP_LONGEST;
        let line_trap_min = width.min(height) * LINE_TRAP_SHORTEST;
        let mut line_trap_count = 0;
        let horiz = dir == BND_LEFT || dir == BND_RIGHT;
        let mut min_good_overlap = if horiz { height / 2 } else { width / 2 };
        let mut min_decent_overlap = if horiz { height / 3 } else { width / 3 };
        if leaders {
            min_good_overlap = 1;
            min_decent_overlap = 1;
        }
        let mut search_pad = ((f64::from(width * height)).sqrt() * NEIGHBOUR_SEARCH_FACTOR) as i32;
        if self.gridsize() > search_pad {
            search_pad = self.gridsize();
        }
        let mut sb = b;
        match dir {
            BND_LEFT => sb.left -= search_pad,
            BND_RIGHT => sb.right += search_pad,
            BND_BELOW => sb.bottom -= search_pad,
            _ => sb.top += search_pad,
        }
        let mut rs = GridSearch::new();
        rs.start_rect_search(&self.sw, &sb);
        let mut best: Option<BlobId> = None;
        let mut best_goodness = 0.0f64;
        let mut best_is_good = false;
        while let Some(n) = rs.next_rect_search(&self.sw, &*blobs) {
            if n == blob {
                continue;
            }
            let nb = blobs.get(n);
            let nbox = nb.bbox;
            let mid_x = (nbox.left + nbox.right) / 2;
            if mid_x < bb.left_rule || mid_x > bb.right_rule {
                continue;
            }
            let (n_width, n_height) = (nbox.width(), nbox.height());
            if n_width.min(n_height) > line_trap_min && n_width.max(n_height) < line_trap_max {
                line_trap_count += 1;
            }
            if TabFind::very_different_sizes(n_width.max(n_height), width.max(height))
                && ((horiz && TabFind::different_sizes(n_height, height))
                    || (!horiz && TabFind::different_sizes(n_width, width)))
            {
                continue;
            }
            let (overlap, perp_overlap, mut gap);
            if horiz {
                overlap = nbox.top.min(top) - nbox.bottom.max(bottom);
                perp_overlap = if overlap == nbox.height() && nbox.width() > nbox.height() {
                    nbox.width()
                } else {
                    overlap
                };
                gap = if dir == BND_LEFT {
                    left - nbox.left
                } else {
                    nbox.right - right
                };
                if gap <= 0 {
                    continue;
                }
                gap -= n_width;
            } else {
                overlap = nbox.right.min(right) - nbox.left.max(left);
                perp_overlap = if overlap == nbox.width() && nbox.height() > nbox.width() {
                    nbox.height()
                } else {
                    overlap
                };
                gap = if dir == BND_BELOW {
                    bottom - nbox.bottom
                } else {
                    nbox.top - top
                };
                if gap <= 0 {
                    continue;
                }
                gap -= n_height;
            }
            if -gap > overlap {
                continue;
            }
            if perp_overlap < min_decent_overlap {
                continue;
            }
            let bad_sizes = TabFind::different_sizes(height, n_height)
                && TabFind::different_sizes(width, n_width);
            let is_good = overlap >= min_good_overlap
                && !bad_sizes
                && bb.matching_stroke_width(
                    nb,
                    STROKE_WIDTH_FRACTION_TOLERANCE,
                    STROKE_WIDTH_TOLERANCE,
                );
            if gap < 1 {
                gap = 1;
            }
            let goodness =
                (1.0 + f64::from(u8::from(is_good))) * f64::from(overlap) / f64::from(gap);
            if goodness > best_goodness {
                best = Some(n);
                best_goodness = goodness;
                best_is_good = is_good;
            }
        }
        blobs.get_mut(blob).set_neighbour(dir, best, best_is_good);
        line_trap_count
    }

    /// `FindTextlineFlowDirection` for horizontal-only modes.
    fn find_textline_flow_direction(&mut self, blobs: &mut Blobs) {
        let mut gs = GridSearch::new();
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            self.set_neighbours(blobs, false, false, b);
        }
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            self.simplify_obvious_neighbours(blobs, b);
        }
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            let bb = blobs.get_mut(b);
            bb.vert_possible = false;
            bb.horz_possible = true;
        }
        for reset_all in [false, true, true] {
            gs.start_full_search(&self.sw);
            while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
                smooth_neighbour_types(blobs, reset_all, b);
            }
        }
    }

    fn simplify_obvious_neighbours(&self, blobs: &mut Blobs, blob: BlobId) {
        let b = blobs.get(blob);
        let bx = b.bbox;
        let asw = b.area_stroke_width;
        if bx.width() as f32 > 3.0 * asw && bx.height() as f32 > 3.0 * asw {
            if bx.width() > 4 * bx.height() {
                let bm = blobs.get_mut(blob);
                bm.set_neighbour(BND_ABOVE, None, false);
                bm.set_neighbour(BND_BELOW, None, false);
                return;
            }
            if bx.height() > 4 * bx.width() {
                let bm = blobs.get_mut(blob);
                bm.set_neighbour(BND_LEFT, None, false);
                bm.set_neighbour(BND_RIGHT, None, false);
                return;
            }
        }
        let margin = self.gridsize() / 2;
        let (h_min, h_max, v_min, v_max) = blobs.min_max_gaps_clipped(blob);
        let b = blobs.get(blob);
        if (h_max + margin < v_min && h_max < margin / 2) || b.leader_on_left || b.leader_on_right {
            let bm = blobs.get_mut(blob);
            bm.set_neighbour(BND_ABOVE, None, false);
            bm.set_neighbour(BND_BELOW, None, false);
        } else if v_max + margin < h_min && v_max < margin / 2 {
            let bm = blobs.get_mut(blob);
            bm.set_neighbour(BND_LEFT, None, false);
            bm.set_neighbour(BND_RIGHT, None, false);
        }
    }

    /// `FindLeaderPartitions`.
    fn find_leader_partitions(&mut self, blobs: &mut Blobs, tb: &mut ToBlock) {
        self.sw.clear();
        let mut leader_parts = self.find_leaders_and_mark_noise(blobs, tb);
        Self::insert_blob_list(&mut self.sw, blobs, &tb.blobs);
        for part in leader_parts.take_all() {
            self.parts.claim_boxes(blobs, part);
            self.mark_leader_neighbours(blobs, part, true);
            self.mark_leader_neighbours(blobs, part, false);
            self.part_grid.insert_bbox(&self.parts, true, true, part);
        }
    }

    fn find_leaders_and_mark_noise(
        &mut self,
        blobs: &mut Blobs,
        tb: &mut ToBlock,
    ) -> EList<PartId> {
        Self::insert_blob_list(&mut self.sw, blobs, &tb.small_blobs);
        Self::insert_blob_list(&mut self.sw, blobs, &tb.noise_blobs);
        let mut gs = GridSearch::new();
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            self.set_neighbours(blobs, true, false, b);
        }
        let mut leader_parts = EList::new();
        let mut part_it = Iter::new(&leader_parts);
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            if blobs.get(b).flow != FlowType::None {
                continue;
            }
            if blobs.get(b).neighbours[BND_RIGHT].is_none()
                && blobs.get(b).neighbours[BND_LEFT].is_none()
            {
                continue;
            }
            let part = self
                .parts
                .add(ColPartition::new(RegionType::Unknown, ICoord::new(0, 1)));
            let mut cur = Some(b);
            while let Some(x) = cur.filter(|&x| blobs.get(x).flow == FlowType::None) {
                self.parts.add_box(blobs, part, x);
                cur = blobs.get(x).neighbours[BND_RIGHT];
            }
            let mut cur = blobs.get(b).neighbours[BND_LEFT];
            while let Some(x) = cur.filter(|&x| blobs.get(x).flow == FlowType::None) {
                self.parts.add_box(blobs, part, x);
                cur = blobs.get(x).neighbours[BND_LEFT];
            }
            if self.parts.mark_as_leader_if_monospaced(blobs, part) {
                part_it.add_after_then_move(&mut leader_parts, part);
            } else {
                self.parts.delete(part);
            }
        }
        let mut blob_it = Iter::new(&tb.blobs);
        let mut small_it = Iter::new(&tb.small_blobs);
        small_it.mark_cycle_pt();
        while !small_it.cycled_list(&tb.small_blobs) {
            let b = small_it.data(&tb.small_blobs);
            if blobs.get(b).flow != FlowType::Leader {
                let bm = blobs.get_mut(b);
                if bm.flow == FlowType::Neighbours {
                    bm.flow = FlowType::None;
                }
                bm.clear_neighbours();
                let v = small_it.extract(&mut tb.small_blobs);
                blob_it.add_to_end(&mut tb.blobs, v);
            }
            small_it.forward(&tb.small_blobs);
        }
        let mut noise_it = Iter::new(&tb.noise_blobs);
        noise_it.mark_cycle_pt();
        while !noise_it.cycled_list(&tb.noise_blobs) {
            let b = noise_it.data(&tb.noise_blobs);
            let bm = blobs.get_mut(b);
            if bm.flow == FlowType::Leader || bm.joined {
                let v = noise_it.extract(&mut tb.noise_blobs);
                small_it.add_to_end(&mut tb.small_blobs, v);
            } else if bm.flow == FlowType::Neighbours {
                bm.flow = FlowType::None;
                bm.clear_neighbours();
            }
            noise_it.forward(&tb.noise_blobs);
        }
        self.sw.clear();
        leader_parts
    }

    /// `MarkLeaderNeighbours`; `left` is `LR_LEFT`.
    fn mark_leader_neighbours(&mut self, blobs: &mut Blobs, part: PartId, left: bool) {
        let pb = self.parts.get(part).bounding_box;
        let mut s = GridSearch::new();
        let mut best: Option<BlobId> = None;
        let mut best_gap = 0;
        s.start_side_search(
            &self.sw,
            if left { pb.left } else { pb.right },
            pb.bottom,
            pb.top,
        );
        while let Some(b) = s.next_side_search(&self.sw, left) {
            let bb = blobs.get(b).bbox;
            if !bb.y_overlap(&pb) {
                continue;
            }
            let x_gap = bb.x_gap(&pb);
            if x_gap > 2 * self.gridsize() {
                break;
            } else if best.is_none() || x_gap < best_gap {
                best = Some(b);
                best_gap = x_gap;
            }
        }
        if let Some(b) = best {
            if left {
                blobs.get_mut(b).leader_on_right = true;
            } else {
                blobs.get_mut(b).leader_on_left = true;
            }
        }
    }

    /// `RemoveLineResidue`.
    fn remove_line_residue(&mut self, blobs: &mut Blobs) {
        let mut gs = GridSearch::new();
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            let bx = blobs.get(b).bbox;
            if f64::from(bx.height()) < f64::from(bx.width()) * LINE_RESIDUE_ASPECT_RATIO {
                continue;
            }
            let padding = bx.height() * LINE_RESIDUE_PAD_RATIO;
            let mut sb = bx;
            sb.pad(padding, padding);
            let mut rs = GridSearch::new();
            let mut max_height = 0;
            rs.start_rect_search(&self.sw, &sb);
            while let Some(n) = rs.next_rect_search(&self.sw, &*blobs) {
                if n == b {
                    continue;
                }
                max_height = max_height.max(blobs.get(n).bbox.height());
            }
            if f64::from(max_height) * LINE_RESIDUE_SIZE_RATIO < f64::from(bx.height()) {
                self.parts
                    .make_big_partition(blobs, b, Some(&mut self.big_parts));
            }
        }
    }

    /// `GradeBlobsIntoPartitions`.
    fn grade_blobs_into_partitions(
        &mut self,
        blobs: &mut Blobs,
        tb: &mut ToBlock,
        diacritic_blobs: &mut EList<BlobId>,
    ) {
        self.sw.clear();
        Self::insert_blob_list(&mut self.sw, blobs, &tb.blobs);
        Self::insert_blob_list(&mut self.sw, blobs, &tb.large_blobs);
        self.find_textline_flow_direction(blobs);
        self.projection.construct_projection(
            blobs,
            [&tb.blobs, &tb.large_blobs],
            &self.nontext_map,
        );
        let mut noise = std::mem::take(&mut tb.noise_blobs);
        self.projection
            .move_non_textline_blobs(blobs, &mut tb.blobs, &mut noise);
        self.projection
            .move_non_textline_blobs(blobs, &mut tb.small_blobs, &mut noise);
        tb.noise_blobs = noise;
        self.sw.clear();
        Self::insert_blob_list(&mut self.sw, blobs, &tb.blobs);
        Self::insert_blob_list(&mut self.sw, blobs, &tb.large_blobs);
        self.find_textline_flow_direction(blobs);
        if self.find_initial_partitions(blobs, true, tb, diacritic_blobs) {
            eprintln!("Detected {} diacritics", diacritic_blobs.len());
            self.sw.clear();
            Self::insert_blob_list(&mut self.sw, blobs, &tb.blobs);
            Self::insert_blob_list(&mut self.sw, blobs, &tb.large_blobs);
            self.find_textline_flow_direction(blobs);
            self.find_initial_partitions(blobs, false, tb, diacritic_blobs);
        }
    }

    /// `FindInitialPartitions`; true for `PFR_NOISE`.
    fn find_initial_partitions(
        &mut self,
        blobs: &mut Blobs,
        find_problems: bool,
        tb: &mut ToBlock,
        diacritic_blobs: &mut EList<BlobId>,
    ) -> bool {
        self.find_horizontal_text_chains(blobs);
        let mut big = std::mem::take(&mut self.big_parts);
        self.pg(blobs).split_overlapping_partitions(&mut big);
        self.big_parts = big;
        self.easy_merges(blobs);
        for b in tb.large_blobs.to_vec() {
            if blobs.get(b).owner.is_none() {
                self.parts
                    .make_big_partition(blobs, b, Some(&mut self.big_parts));
            }
        }
        let grid_box = self.grid_box;
        self.smooth_all(blobs, &[FlowType::Chain, FlowType::Neighbours], &grid_box);
        let (pre_overlap, _) = self.pg(blobs).compute_total_overlap(false);
        self.test_diacritics(blobs, tb);
        self.merge_diacritics(blobs, tb);
        if find_problems
            && self.detect_and_remove_noise(blobs, pre_overlap, &grid_box, tb, diacritic_blobs)
        {
            return true;
        }
        self.partition_remaining_blobs(blobs);
        let mut big = std::mem::take(&mut self.big_parts);
        self.pg(blobs).split_overlapping_partitions(&mut big);
        self.big_parts = big;
        self.easy_merges(blobs);
        self.smooth_all(
            blobs,
            &[FlowType::Chain, FlowType::Neighbours, FlowType::StrongChain],
            &grid_box,
        );
        false
    }

    fn smooth_all(&mut self, blobs: &mut Blobs, types: &[FlowType], grid_box: &TBox) {
        let map = std::mem::replace(&mut self.nontext_map, Bitmap::new(0, 0));
        for &t in types {
            while self.pg(blobs).grid_smooth_neighbours(t, &map, grid_box) {}
        }
        self.nontext_map = map;
    }

    /// `DetectAndRemoveNoise`.
    fn detect_and_remove_noise(
        &mut self,
        blobs: &mut Blobs,
        pre_overlap: i32,
        grid_box: &TBox,
        tb: &mut ToBlock,
        diacritic_blobs: &mut EList<BlobId>,
    ) -> bool {
        let (post_overlap, noise_grid) = self.pg(blobs).compute_total_overlap(true);
        let pre_overlap = if pre_overlap == 0 { 1 } else { pre_overlap };
        let mut diacritic_it = Iter::new(diacritic_blobs);
        let Some(mut noise_grid) = noise_grid else {
            return false;
        };
        let result = f64::from(post_overlap) > f64::from(pre_overlap) * NOISE_OVERLAP_GROWTH_FACTOR
            && f64::from(post_overlap) > f64::from(grid_box.area()) * NOISE_OVERLAP_AREA_FACTOR;
        if result {
            self.pg(blobs).delete_non_leader_parts();
            let gs = self.gridsize();
            let mut it = Iter::new(&tb.noise_blobs);
            it.mark_cycle_pt();
            while !it.cycled_list(&tb.noise_blobs) {
                let b = it.data(&tb.noise_blobs);
                blobs.get_mut(b).clear_neighbours();
                let bb = blobs.get(b);
                if bb.is_diacritic() && bb.owner.is_none() {
                    let mut sb = bb.bbox;
                    sb.pad(gs, gs);
                    let mut rs = GridSearch::new();
                    rs.start_rect_search(&noise_grid, &sb);
                    if rs.next_rect_search(&noise_grid, &self.parts).is_some() {
                        blobs.get_mut(b).compute_bounding_box();
                        let v = it.extract(&mut tb.noise_blobs);
                        diacritic_it.add_after_then_move(diacritic_blobs, v);
                    }
                }
                it.forward(&tb.noise_blobs);
            }
        }
        // noise_grid->DeleteParts(): the copies own no blobs.
        let mut ops = PartGridOps {
            grid: &mut noise_grid,
            parts: &mut self.parts,
            blobs,
        };
        ops.delete_parts();
        result
    }

    /// `FindHorizontalTextChains`.
    fn find_horizontal_text_chains(&mut self, blobs: &mut Blobs) {
        let mutual_h = |blobs: &Blobs, blob: BlobId, dir: usize| -> Option<BlobId> {
            let n = blobs.get(blob).neighbours[dir]?;
            let nb = blobs.get(n);
            if nb.owner.is_some() || nb.uniquely_vertical() {
                return None;
            }
            (nb.neighbours[dir ^ 2] == Some(blob)).then_some(n)
        };
        let mutual_v = |blobs: &Blobs, blob: BlobId, dir: usize| -> Option<BlobId> {
            let n = blobs.get(blob).neighbours[dir]?;
            let nb = blobs.get(n);
            if nb.owner.is_some() || nb.uniquely_horizontal() {
                return None;
            }
            (nb.neighbours[dir ^ 2] == Some(blob)).then_some(n)
        };
        let mut gs = GridSearch::new();
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            let bb = blobs.get(b);
            if bb.owner.is_some() || !bb.uniquely_horizontal() {
                continue;
            }
            let Some(first) = mutual_h(blobs, b, BND_RIGHT) else {
                continue;
            };
            let part = self
                .parts
                .add(ColPartition::new(RegionType::Text, ICoord::new(0, 1)));
            self.parts.add_box(blobs, part, b);
            let mut cur = Some(first);
            while let Some(x) = cur {
                self.parts.add_box(blobs, part, x);
                cur = mutual_h(blobs, x, BND_RIGHT);
            }
            // Tesseract continues leftwards with the vertical helper.
            let mut cur = mutual_h(blobs, b, BND_LEFT);
            while let Some(x) = cur {
                self.parts.add_box(blobs, part, x);
                cur = mutual_v(blobs, x, BND_LEFT);
            }
            self.complete_partition(blobs, part);
        }
    }

    /// `CompletePartition` (horizontal-only modes).
    fn complete_partition(&mut self, blobs: &mut Blobs, part: PartId) {
        self.parts.compute_limits(blobs, part);
        let mut value = self.projection.evaluate_col_partition(&self.parts, part);
        if value < 0 {
            value = if self.parts.get(part).boxes_count() == 1 {
                0
            } else {
                2
            };
        }
        self.parts
            .set_region_and_flow_types_from_projection_value(blobs, part, value);
        self.parts.claim_boxes(blobs, part);
        self.part_grid.insert_bbox(&self.parts, true, true, part);
    }

    /// `EasyMerges`.
    fn easy_merges(&mut self, blobs: &mut Blobs) {
        let map = std::mem::replace(&mut self.nontext_map, Bitmap::new(0, 0));
        let grid_box = self.grid_box;
        let box_cb = |parts: &Parts, part: PartId, b: &mut TBox| -> bool {
            if parts.get(part).is_vertical_type() {
                b.top += b.width();
                b.bottom -= b.width();
            } else {
                b.left -= b.height();
                b.right += b.height();
            }
            true
        };
        let confirm_cb = |parts: &Parts, blobs: &Blobs, a: PartId, b: PartId| -> bool {
            let (p1, p2) = (parts.get(a), parts.get(b));
            if (p1.flow == FlowType::NonText && p2.flow >= FlowType::Chain)
                || (p1.flow >= FlowType::Chain && p2.flow == FlowType::NonText)
            {
                return false;
            }
            if (p1.is_vertical_type() || p2.is_vertical_type())
                && p1.hcore_overlap(p2) <= 0
                && ((!p1.is_singleton() && !p2.is_singleton())
                    || !p1.bounding_box.major_overlap(&p2.bounding_box))
            {
                return false;
            }
            if (p1.is_horizontal_type() || p2.is_horizontal_type())
                && p1.vcore_overlap(p2) <= 0
                && ((!p1.is_singleton() && !p2.is_singleton())
                    || (!p1.bounding_box.major_overlap(&p2.bounding_box)
                        && !parts.ok_diacritic_merge(blobs, a, b)
                        && !parts.ok_diacritic_merge(blobs, b, a)))
            {
                return false;
            }
            if !parts.confirm_no_tab_violation(blobs, a, b) {
                return false;
            }
            if p1.flow <= FlowType::NonText && p2.flow <= FlowType::NonText {
                return true;
            }
            blank_image_in_between(&p1.bounding_box, &p2.bounding_box, &grid_box, &map)
        };
        self.pg(blobs).merges(&box_cb, &confirm_cb);
        self.nontext_map = map;
    }

    /// `TestDiacritics`.
    fn test_diacritics(&mut self, blobs: &mut Blobs, tb: &mut ToBlock) {
        let mut small_grid: BBGrid<BlobId> =
            BBGrid::new(self.gridsize(), self.tf.bleft(), self.tf.tright());
        Self::insert_blob_list(&mut small_grid, blobs, &tb.noise_blobs);
        Self::insert_blob_list(&mut small_grid, blobs, &tb.blobs);
        for b in tb.noise_blobs.to_vec() {
            let bb = blobs.get(b);
            if bb.owner.is_none() && !bb.is_diacritic() {
                self.diacritic_blob(blobs, &small_grid, b);
            }
        }
        let mut small_it = Iter::new(&tb.noise_blobs);
        let mut blob_it = Iter::new(&tb.blobs);
        blob_it.mark_cycle_pt();
        while !blob_it.cycled_list(&tb.blobs) {
            let b = blob_it.data(&tb.blobs);
            if blobs.get(b).is_diacritic() {
                let v = blob_it.extract(&mut tb.blobs);
                small_it.add_to_end(&mut tb.noise_blobs, v);
                blob_it.forward(&tb.blobs);
                continue;
            }
            let owner = blobs.get(b).owner;
            match owner {
                None => {
                    if self.diacritic_blob(blobs, &small_grid, b) {
                        self.sw.remove_bbox(&*blobs, b);
                        let v = blob_it.extract(&mut tb.blobs);
                        small_it.add_to_end(&mut tb.noise_blobs, v);
                    }
                }
                Some(part) => {
                    let p = self.parts.get(part);
                    if !p.block_owned && p.boxes_count() < 3 {
                        let boxes = p.boxes.to_vec();
                        let mut all = true;
                        for &x in &boxes {
                            if !self.diacritic_blob(blobs, &small_grid, x) {
                                all = false;
                                break;
                            }
                        }
                        if all {
                            for x in self.parts.get_mut(part).boxes.take_all() {
                                blobs.get_mut(x).owner = None;
                                self.sw.remove_bbox(&*blobs, x);
                            }
                            let v = blob_it.extract(&mut tb.blobs);
                            small_it.add_to_end(&mut tb.noise_blobs, v);
                            self.part_grid.remove_bbox(&self.parts, part);
                            self.parts.delete(part);
                        }
                    }
                }
            }
            blob_it.forward(&tb.blobs);
        }
    }

    /// `DiacriticBlob`.
    fn diacritic_blob(
        &mut self,
        blobs: &mut Blobs,
        small_grid: &BBGrid<BlobId>,
        blob: BlobId,
    ) -> bool {
        let bb = blobs.get(blob);
        if unmergeable(bb.region_type) || bb.region_type == RegionType::VertText {
            return false;
        }
        let small_box = bb.bbox;
        let height = small_box.height();
        let mut best_x_overlap: Option<BlobId> = None;
        let mut best_y_overlap: Option<BlobId> = None;
        let mut best_total_dist = 0;
        let mut best_y_gap = 0;
        let mut best_xbox = TBox::default();
        let mut sb = small_box;
        let x_pad = int_cast_rounded(f64::from(self.gridsize()) * DIACRITIC_X_PAD_RATIO);
        let y_pad = int_cast_rounded(f64::from(self.gridsize()) * DIACRITIC_Y_PAD_RATIO);
        sb.pad(x_pad, y_pad);
        let mut rs = GridSearch::new();
        rs.set_unique_mode(true);
        let min_height = (f64::from(height) * MIN_DIACRITIC_SIZE_RATIO) as i32;
        rs.start_rect_search(&self.sw, &sb);
        let blob_owner = bb.owner;
        while let Some(n) = rs.next_rect_search(&self.sw, &*blobs) {
            let nb = blobs.get(n);
            if unmergeable(nb.region_type) || n == blob || nb.owner == blob_owner {
                continue;
            }
            let mut nbox = nb.bbox;
            let Some(n_owner) = nb.owner else {
                continue;
            };
            if self.parts.get(n_owner).is_vertical_type()
                || (nb.flow != FlowType::Chain && nb.flow != FlowType::StrongChain)
            {
                continue;
            }
            if nbox.height() < min_height {
                continue;
            }
            let x_gap = small_box.x_gap(&nbox);
            let total_distance = self
                .projection
                .distance_of_box_from_box(&small_box, &nbox, true);
            if f64::from(total_distance)
                > f64::from(self.parts.get(n_owner).median_height) * MAX_DIACRITIC_DISTANCE_RATIO
            {
                continue;
            }
            if x_gap <= 0 {
                let left = small_box.left - small_box.width();
                let right = small_box.right + small_box.width();
                nbox = bounds_within_limits(blobs, n, left, right);
                let y_gap = small_box.y_gap(&nbox);
                if best_x_overlap.is_none() || y_gap < best_y_gap {
                    best_x_overlap = Some(n);
                    best_xbox = nbox;
                    best_y_gap = y_gap;
                }
            } else if blobs.get(blob).confirm_no_tab_violation(blobs.get(n))
                && (best_y_overlap.is_none() || total_distance < best_total_dist)
            {
                best_y_overlap = Some(n);
                best_total_dist = total_distance;
            }
        }
        if let Some(bx) = best_x_overlap
            && best_y_overlap.is_none_or(|by| best_xbox.major_y_overlap(&blobs.get(by).bbox))
        {
            let bm = blobs.get_mut(blob);
            bm.set_diacritic_box(&best_xbox);
            bm.base_char_blob = Some(bx);
            return true;
        }
        if let Some(by) = best_y_overlap {
            let base_box = blobs.get(by).bbox;
            if diacritic_x_gap_filled(blobs, small_grid, &small_box, &base_box)
                && blank_image_in_between(&small_box, &base_box, &self.grid_box, &self.nontext_map)
            {
                let bm = blobs.get_mut(blob);
                bm.set_diacritic_box(&base_box);
                bm.base_char_blob = Some(by);
                return true;
            }
        }
        false
    }

    /// `MergeDiacritics`.
    fn merge_diacritics(&mut self, blobs: &mut Blobs, tb: &ToBlock) {
        for b in tb.noise_blobs.to_vec() {
            if let Some(base) = blobs.get(b).base_char_blob {
                if let Some(part) = blobs.get(base).owner
                    && !self.parts.get(part).block_owned
                    && blobs.get(b).owner.is_none()
                    && blobs.get(b).is_diacritic()
                {
                    self.part_grid.remove_bbox(&self.parts, part);
                    self.parts.add_box(blobs, part, b);
                    let p = self.parts.get(part);
                    let (bt, fl) = (p.blob_type, p.flow);
                    let bm = blobs.get_mut(b);
                    bm.region_type = bt;
                    bm.flow = fl;
                    bm.owner = Some(part);
                    self.part_grid.insert_bbox(&self.parts, true, true, part);
                }
                blobs.get_mut(b).base_char_blob = None;
            }
        }
    }

    /// `PartitionRemainingBlobs`.
    fn partition_remaining_blobs(&mut self, blobs: &mut Blobs) {
        let mut gs = GridSearch::new();
        let mut prev = (-1, -1);
        let mut cell_list: EList<BlobId> = EList::new();
        let mut cell_it = Iter::new(&cell_list);
        let mut cell_all_noise = true;
        gs.start_full_search(&self.sw);
        while let Some(b) = gs.next_full_search(&self.sw, &*blobs) {
            let cur = (gs.x, gs.y);
            if cur != prev {
                self.make_partitions_from_cell_list(blobs, cell_all_noise, &mut cell_list);
                cell_it = Iter::new(&cell_list);
                prev = cur;
                cell_all_noise = true;
            }
            if blobs.get(b).owner.is_none() {
                cell_it.add_to_end(&mut cell_list, b);
                if blobs.get(b).flow != FlowType::NonText {
                    cell_all_noise = false;
                }
            } else {
                cell_all_noise = false;
            }
        }
        self.make_partitions_from_cell_list(blobs, cell_all_noise, &mut cell_list);
    }

    fn make_partitions_from_cell_list(
        &mut self,
        blobs: &mut Blobs,
        combine: bool,
        cell_list: &mut EList<BlobId>,
    ) {
        if cell_list.is_empty() {
            return;
        }
        let items = cell_list.take_all();
        if combine {
            let first = items[0];
            let part = self.parts.add(ColPartition::new(
                blobs.get(first).region_type,
                ICoord::new(0, 1),
            ));
            self.parts.add_box(blobs, part, first);
            self.parts.get_mut(part).flow = blobs.get(first).flow;
            for &b in &items[1..] {
                self.parts.add_box(blobs, part, b);
            }
            self.complete_partition(blobs, part);
        } else {
            for b in items {
                let part = self.parts.add(ColPartition::new(
                    blobs.get(b).region_type,
                    ICoord::new(0, 1),
                ));
                self.parts.get_mut(part).flow = blobs.get(b).flow;
                self.parts.add_box(blobs, part, b);
                self.complete_partition(blobs, part);
            }
        }
    }
}

/// `SmoothNeighbourTypes` for horizontal-only modes.
fn smooth_neighbour_types(blobs: &mut Blobs, reset_all: bool, blob: BlobId) {
    let b = blobs.get(blob);
    if (b.vert_possible && b.horz_possible) || reset_all {
        let mut neighbours = EList::new();
        list_2nd_neighbours(blobs, blob, &mut neighbours);
        let mut pure_h = 0;
        let mut pure_v = 0;
        for n in neighbours.to_vec() {
            let nb = blobs.get(n);
            if nb.uniquely_horizontal() {
                pure_h += 1;
            }
            if nb.uniquely_vertical() {
                pure_v += 1;
            }
        }
        if pure_h > pure_v {
            let bm = blobs.get_mut(blob);
            bm.vert_possible = false;
            bm.horz_possible = true;
        }
    }
}

/// `BLOBNBOX::BoundsWithinLimits` (no rotation).
fn bounds_within_limits(blobs: &Blobs, id: BlobId, left: i32, right: i32) -> TBox {
    let b = blobs.get(id);
    let mut top = b.bbox.top as f32;
    let mut bottom = b.bbox.bottom as f32;
    if let Some(c) = &b.cblob {
        let (bt, tp) = super::outline::find_cblob_limits(c, left as f32, right as f32);
        bottom = bt;
        top = tp;
    }
    if top < bottom {
        top = b.bbox.top as f32;
        bottom = b.bbox.bottom as f32;
    }
    let bl = TBox::new(left, bottom.floor() as i32, left, bottom.ceil() as i32);
    let tr = TBox::new(right, top.floor() as i32, right, top.ceil() as i32);
    let mut r = bl;
    r.union_with(&tr);
    r
}

/// `DiacriticXGapFilled`.
fn diacritic_x_gap_filled(
    blobs: &Blobs,
    grid: &BBGrid<BlobId>,
    diacritic_box: &TBox,
    base_box: &TBox,
) -> bool {
    let max_gap =
        int_cast_rounded(f64::from(base_box.height()) * MAX_DIACRITIC_GAP_TO_BASE_CHAR_HEIGHT);
    let mut occupied = *base_box;
    loop {
        let diacritic_gap = diacritic_box.x_gap(&occupied);
        if diacritic_gap <= max_gap {
            return true;
        }
        let mut sb = occupied;
        if diacritic_box.left > sb.right {
            sb.left = sb.right;
            sb.right = sb.left + max_gap;
        } else {
            sb.right = sb.left;
            sb.left -= max_gap;
        }
        let mut rs = GridSearch::new();
        rs.start_rect_search(grid, &sb);
        let mut found = false;
        while let Some(n) = rs.next_rect_search(grid, blobs) {
            let nb = blobs.get(n).bbox;
            if nb.x_gap(diacritic_box) < diacritic_gap {
                if nb.left < occupied.left {
                    occupied.left = nb.left;
                }
                if nb.right > occupied.right {
                    occupied.right = nb.right;
                }
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }
}

/// `CCNonTextDetect::ComputeNonTextMask`.
fn compute_non_text_mask(
    gridsize: i32,
    bleft: ICoord,
    tright: ICoord,
    blobs: &mut Blobs,
    photo_map: &Bitmap,
    tb: &mut ToBlock,
) -> Bitmap {
    let max_noise_count = (MAX_SMALL_NEIGHBOURS_PER_PIX * f64::from(gridsize * gridsize)) as i32;
    let mut grid: BBGrid<BlobId> = BBGrid::new(gridsize, bleft, tright);
    let ins = |g: &mut BBGrid<BlobId>, blobs: &Blobs, list: &EList<BlobId>| {
        for b in list.to_vec() {
            if !blobs.get(b).joined {
                g.insert_bbox(blobs, true, true, b);
            }
        }
    };
    ins(&mut grid, blobs, &tb.small_blobs);
    ins(&mut grid, blobs, &tb.noise_blobs);
    let mut good_grid: BBGrid<BlobId> = BBGrid::new(gridsize, bleft, tright);
    for b in tb.blobs.to_vec() {
        let bb = blobs.get(b);
        let perimeter = bb.cblob.as_ref().map_or(0, |c| c.perimeter());
        let mut pa_ratio = f64::from(perimeter) / 4.0;
        pa_ratio *= pa_ratio / f64::from(bb.area);
        if bb.good_text_blob() == 0 || pa_ratio < MIN_GOOD_TEXT_PA_RATIO {
            grid.insert_bbox(&*blobs, true, true, b);
        } else {
            good_grid.insert_bbox(&*blobs, true, true, b);
        }
    }
    let noise_density = compute_noise_density(&grid, &good_grid, max_noise_count, photo_map);
    let mut pix = noise_density.threshold_to_pix(max_noise_count);
    let imageheight = tright.y - bleft.x;
    let ctx = NonText {
        gridsize,
        max_noise_count,
        noise_density: &noise_density,
        imageheight,
    };
    ctx.mark_and_delete(
        blobs,
        &mut grid,
        &mut tb.large_blobs,
        MAX_LARGE_OVERLAPS_WITH_SMALL,
        &mut pix,
    );
    ctx.mark_and_delete(
        blobs,
        &mut grid,
        &mut tb.blobs,
        MAX_MEDIUM_OVERLAPS_WITH_SMALL,
        &mut pix,
    );
    grid.clear();
    ins(&mut grid, blobs, &tb.blobs);
    ctx.mark_and_delete(
        blobs,
        &mut grid,
        &mut tb.large_blobs,
        MAX_LARGE_OVERLAPS_WITH_MEDIUM,
        &mut pix,
    );
    grid.clear();
    ctx.mark_and_delete(blobs, &mut grid, &mut tb.noise_blobs, -1, &mut pix);
    ctx.mark_and_delete(blobs, &mut grid, &mut tb.small_blobs, -1, &mut pix);
    ctx.mark_and_delete(blobs, &mut grid, &mut tb.blobs, -1, &mut pix);
    pix
}

fn compute_noise_density(
    grid: &BBGrid<BlobId>,
    good_grid: &BBGrid<BlobId>,
    max_noise_count: i32,
    photo_map: &Bitmap,
) -> IntGrid {
    let noise_counts = grid.count_cell_elements();
    let mut noise_density = noise_counts.neighbourhood_sum();
    let good_counts = good_grid.count_cell_elements();
    let height = photo_map.height as i32;
    let photo_offset = int_cast_rounded(f64::from(max_noise_count) * PHOTO_OFFSET_FRACTION);
    let b = &grid.base;
    for y in 0..b.gridheight {
        for x in 0..b.gridwidth {
            let noise = noise_density.cell(x, y);
            if max_noise_count < noise + photo_offset && noise <= max_noise_count {
                let left = x * b.gridsize;
                let right = left + b.gridsize;
                let bottom = height - y * b.gridsize;
                let top = bottom - b.gridsize;
                // ImageFind::BoundsWithinRect
                let input = box_create(left, top, right - left, bottom - top);
                if clip_box_to_foreground(photo_map, input).is_some() {
                    noise_density.set_cell(x, y, noise + photo_offset);
                }
            }
            if noise > max_noise_count
                && good_counts.cell(x, y) > 0
                && noise_counts.cell(x, y) * ORIGINAL_NOISE_MULTIPLE <= max_noise_count
            {
                noise_density.set_cell(x, y, 0);
            }
        }
    }
    noise_density
}

struct NonText<'a> {
    gridsize: i32,
    max_noise_count: i32,
    noise_density: &'a IntGrid,
    imageheight: i32,
}

impl NonText<'_> {
    fn attempt_box_expansion(&self, b: &TBox, pad: i32) -> TBox {
        let nd = self.noise_density;
        let mut e = *b;
        e.right = b.right + pad;
        if !nd.any_zero_in_rect(&e) {
            return e;
        }
        e = *b;
        e.left = b.left - pad;
        if !nd.any_zero_in_rect(&e) {
            return e;
        }
        e = *b;
        e.top = b.top + pad;
        if !nd.any_zero_in_rect(&e) {
            return e;
        }
        e = *b;
        e.bottom = b.bottom + pad;
        if !nd.any_zero_in_rect(&e) {
            return e;
        }
        e = *b;
        e.pad(NOISE_PADDING, NOISE_PADDING);
        if !nd.any_zero_in_rect(&e) {
            return e;
        }
        *b
    }

    fn overlaps_too_much(
        &self,
        blobs: &Blobs,
        grid: &BBGrid<BlobId>,
        blob: BlobId,
        max_overlaps: i32,
    ) -> bool {
        let b = blobs.get(blob).bbox;
        let mut rs = GridSearch::new();
        rs.start_rect_search(grid, &b);
        rs.set_unique_mode(true);
        let mut count = 0;
        while count <= max_overlaps {
            let Some(n) = rs.next_rect_search(grid, blobs) else {
                break;
            };
            if b.major_overlap(&blobs.get(n).bbox) {
                count += 1;
                if count > max_overlaps {
                    return true;
                }
            }
        }
        false
    }

    fn mark_and_delete(
        &self,
        blobs: &mut Blobs,
        grid: &mut BBGrid<BlobId>,
        list: &mut EList<BlobId>,
        max_overlaps: i32,
        mask: &mut Bitmap,
    ) {
        let mut it = Iter::new(list);
        it.mark_cycle_pt();
        while !it.cycled_list(list) {
            let blob = it.data(list);
            let mut b = blobs.get(blob).bbox;
            if !self
                .noise_density
                .rect_mostly_over_threshold(&b, self.max_noise_count)
                && (max_overlaps < 0 || !self.overlaps_too_much(blobs, grid, blob, max_overlaps))
            {
                blobs.get_mut(blob).clear_neighbours();
            } else {
                if self.noise_density.any_zero_in_rect(&b) {
                    if let Some(c) = &blobs.get(blob).cblob {
                        let render = super::outline::render_outline(c);
                        let (dx, dy) = (b.left, self.imageheight - b.top);
                        for y in 0..render.height {
                            for x in 0..render.width {
                                if render.get(x, y) {
                                    let (tx, ty) = (dx + x as i32, dy + y as i32);
                                    if tx >= 0
                                        && ty >= 0
                                        && (tx as usize) < mask.width
                                        && (ty as usize) < mask.height
                                    {
                                        mask.set(tx as usize, ty as usize);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    if b.area() < self.gridsize * self.gridsize {
                        b = self.attempt_box_expansion(&b, self.gridsize);
                    }
                    mask.set_rect(&LBox {
                        x: b.left,
                        y: self.imageheight - b.top,
                        w: b.width(),
                        h: b.height(),
                    });
                }
                blobs.delete(blob);
                it.extract(list);
            }
            it.forward(list);
        }
    }
}

/// `ColumnFinder::RotateAndReskewBlocks` (no rotation): numbers the blocks,
/// splits multi-outline blobs and sets the median blob size.
fn rotate_and_reskew_blocks(blobs: &mut Blobs, out: &mut [LayoutBlock]) {
    for (i, lb) in out.iter_mut().enumerate() {
        lb.index = i as i32 + 1;
        let mut widths = Stats::new(0, lb.bbox.width() - 1);
        let mut heights = Stats::new(0, lb.bbox.height() - 1);
        rotate_and_explode_blob_list(blobs, &mut lb.row.blobs, &mut widths, &mut heights);
        lb.median_size = ICoord::new(
            (widths.median() + 0.5) as i32,
            (heights.median() + 0.5) as i32,
        );
    }
}

fn rotate_and_explode_blob_list(
    blobs: &mut Blobs,
    list: &mut EList<BlobId>,
    widths: &mut Stats,
    heights: &mut Stats,
) {
    let mut it = Iter::new(list);
    it.mark_cycle_pt();
    while !it.cycled_list(list) {
        let blob = it.data(list);
        let n_outlines = blobs
            .get(blob)
            .cblob
            .as_ref()
            .map_or(0, |c| c.outlines.len());
        if n_outlines != 1 {
            let outlines = blobs
                .get_mut(blob)
                .cblob
                .take()
                .map(|c| c.outlines)
                .unwrap_or_default();
            for o in outlines {
                let nb = super::blobbox::BlobNBox::new(super::outline::CBlob { outlines: vec![o] });
                let id = blobs.add(nb);
                it.add_after_stay_put(list, id);
            }
            it.extract(list);
            blobs.delete(blob);
        } else {
            blobs.get_mut(blob).compute_bounding_box();
            let b = blobs.get(blob).bbox;
            widths.add(b.width(), 1);
            heights.add(b.height(), 1);
        }
        it.forward(list);
    }
}

#[allow(dead_code)]
fn sort_blobs_left(blobs: &Blobs, list: &mut EList<BlobId>, b: BlobId) {
    clist_add_sorted(
        list,
        |a, c| sort_by_box_left(&blobs.get(a).bbox, &blobs.get(c).bbox),
        true,
        b,
    );
}
