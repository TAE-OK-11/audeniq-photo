//! `ColPartitionGrid` operations used by the sparse layout path
//! (`colpartitiongrid.cpp`).

use super::bitmap::Bitmap;
use super::blobbox::{
    BND_ABOVE, BND_BELOW, BND_LEFT, BND_RIGHT, BlobId, Blobs, FlowType, RegionType,
};
use super::colpartition::{PartId, Parts, is_line_type};
use super::elist::{EList, Iter};
use super::geom::{ICoord, TBox};
use super::grid::{BBGrid, BoxOf, GridSearch, clist_add_sorted, sort_by_box_left};

const MAX_PAD_FACTOR: i32 = 6;
const MAX_NEIGHBOUR_DIST_FACTOR: i32 = 4;
const BIG_PART_SIZE_RATIO: f64 = 1.75;
const TINY_ENOUGH_TEXTLINE_OVERLAP_FRACTION: f64 = 0.25;
const SMOOTH_DECISION_MARGIN: i32 = 4;

const NPT_HTEXT: usize = 0;
const NPT_VTEXT: usize = 1;
const NPT_WEAK_HTEXT: usize = 2;
const NPT_WEAK_VTEXT: usize = 3;
const NPT_IMAGE: usize = 4;
const NPT_COUNT: usize = 5;

pub type PartGrid = BBGrid<PartId>;

/// `ConfirmEasyMerge`-style merge confirmation.
pub type ConfirmCb<'a> = &'a dyn Fn(&Parts, &Blobs, PartId, PartId) -> bool;

fn part_left_cmp(parts: &Parts) -> impl Fn(PartId, PartId) -> i32 + '_ {
    move |a, b| sort_by_box_left(&parts.box_of(a), &parts.box_of(b))
}

/// `ImageFind::CountPixelsInRotatedBox` without rotation.
pub fn count_pixels_in_box(b: TBox, im_box: &TBox, pix: &Bitmap) -> i32 {
    let b = if b.overlap(im_box) {
        b.intersection(im_box)
    } else {
        return 0;
    };
    if b.null_box() {
        return 0;
    }
    let (w, h) = (b.width(), b.height());
    let sx = b.left - im_box.left;
    let sy = im_box.top - b.top;
    let mut n = 0;
    for y in 0..h {
        let yy = sy + y;
        if yy < 0 || yy >= pix.height as i32 {
            continue;
        }
        for x in 0..w {
            let xx = sx + x;
            if xx >= 0 && xx < pix.width as i32 && pix.get(xx as usize, yy as usize) {
                n += 1;
            }
        }
    }
    n
}

/// `ImageFind::BlankImageInBetween` without rotation.
pub fn blank_image_in_between(b1: &TBox, b2: &TBox, im_box: &TBox, pix: &Bitmap) -> bool {
    let mut sb = *b1;
    sb.union_with(b2);
    if b1.x_gap(b2) >= b1.y_gap(b2) {
        if b1.x_gap(b2) <= 0 {
            return true;
        }
        sb.left = b1.right.min(b2.right);
        sb.right = b1.left.max(b2.left);
    } else {
        if b1.y_gap(b2) <= 0 {
            return true;
        }
        sb.top = b1.bottom.max(b2.bottom);
        sb.bottom = b1.top.min(b2.top);
    }
    count_pixels_in_box(sb, im_box, pix) == 0
}

/// `CLIST::set_subtract`.
fn set_subtract(
    parts: &Parts,
    minuend: &EList<PartId>,
    subtrahend: &EList<PartId>,
) -> EList<PartId> {
    let cmp = part_left_cmp(parts);
    let mut out = EList::new();
    let mut s_it = Iter::new(subtrahend);
    for minu in minuend.to_vec() {
        let mut subtra = None;
        if !subtrahend.is_empty() {
            let mut s = s_it.data(subtrahend);
            while !s_it.at_last(subtrahend) && cmp(s, minu) < 0 {
                s_it.forward(subtrahend);
                s = s_it.data(subtrahend);
            }
            subtra = Some(s);
        }
        if subtra.is_none_or(|s| cmp(s, minu) != 0) {
            clist_add_sorted(&mut out, &cmp, true, minu);
        }
    }
    out
}

/// `OKMergeCandidate`.
pub fn ok_merge_candidate(parts: &Parts, blobs: &Blobs, part: PartId, candidate: PartId) -> bool {
    if candidate == part {
        return false;
    }
    let (p, c) = (parts.get(part), parts.get(candidate));
    if !super::colpartition::ColPartition::types_match(p.blob_type, c.blob_type)
        || c.is_unmergeable_type()
    {
        return false;
    }
    let (pb, cb) = (p.bounding_box, c.bounding_box);
    if c.is_vertical_type() || p.is_vertical_type() {
        let h_dist = -p.hcore_overlap(c);
        if h_dist >= pb.width().max(cb.width()) / 2 {
            return false;
        }
    } else {
        let v_dist = -p.vcore_overlap(c);
        if v_dist >= pb.height().max(cb.height()) / 2 {
            return false;
        }
        if !p.vsignificant_core_overlap(c)
            && !parts.ok_diacritic_merge(blobs, part, candidate)
            && !parts.ok_diacritic_merge(blobs, candidate, part)
        {
            return false;
        }
    }
    true
}

fn increase_in_overlap(
    parts: &Parts,
    m1: PartId,
    m2: PartId,
    ok_overlap: i32,
    list: &EList<PartId>,
) -> i32 {
    let mut total = 0;
    let mut merged = parts.box_of(m1);
    merged.union_with(&parts.box_of(m2));
    for part in list.to_vec() {
        if part == m1 || part == m2 {
            continue;
        }
        let pb = parts.box_of(part);
        let mut overlap_area = pb.intersection(&merged).area();
        if overlap_area > 0 && !parts.ok_merge_overlap(part, m1, m2, ok_overlap) {
            total += overlap_area;
            overlap_area = pb.intersection(&parts.box_of(m1)).area();
            if overlap_area > 0 {
                total -= overlap_area;
            }
            let mut ib = pb.intersection(&parts.box_of(m2));
            overlap_area = ib.area();
            if overlap_area > 0 {
                total -= overlap_area;
                let m1b = parts.box_of(m1);
                ib = if ib.overlap(&m1b) {
                    ib.intersection(&m1b)
                } else {
                    TBox::new(
                        i32::from(i16::MAX),
                        i32::from(i16::MAX),
                        -i32::from(i16::MAX),
                        -i32::from(i16::MAX),
                    )
                };
                overlap_area = ib.area();
                if overlap_area > 0 {
                    total += overlap_area;
                }
            }
        }
    }
    total
}

fn test_compatible_candidates(
    parts: &Parts,
    blobs: &Blobs,
    part: PartId,
    candidates: &EList<PartId>,
) -> bool {
    let c = candidates.to_vec();
    for (i, &candidate) in c.iter().enumerate() {
        if !parts.ok_diacritic_merge(blobs, candidate, part) {
            // ColPartition_C_IT it2(it): a copy starting at the same element,
            // cycling the whole list.
            for k in 0..c.len() {
                let candidate2 = c[(i + k) % c.len()];
                if candidate2 != candidate
                    && !ok_merge_candidate(parts, blobs, candidate, candidate2)
                {
                    return false;
                }
            }
        }
    }
    true
}

fn compute_search_box_and_scaling(
    direction: usize,
    part_box: &TBox,
    min_padding: i32,
) -> (TBox, ICoord) {
    let mut sb = *part_box;
    let mut padding = part_box.height().min(part_box.width());
    padding = padding.max(min_padding);
    padding *= MAX_PAD_FACTOR;
    sb.pad(padding, padding);
    let scaling = match direction {
        BND_LEFT => {
            sb.left = part_box.left;
            ICoord::new(2, 1)
        }
        BND_BELOW => {
            sb.bottom = part_box.bottom;
            ICoord::new(1, 2)
        }
        BND_RIGHT => {
            sb.right = part_box.right;
            ICoord::new(2, 1)
        }
        _ => {
            sb.top = part_box.top;
            ICoord::new(1, 2)
        }
    };
    (sb, scaling)
}

/// Grid-level partition operations. `grid` is the partition grid being
/// worked on; the strokewidth hooks for `Merges` are passed as closures.
pub struct PartGridOps<'a> {
    pub grid: &'a mut PartGrid,
    pub parts: &'a mut Parts,
    pub blobs: &'a mut Blobs,
}

impl PartGridOps<'_> {
    fn gridsize(&self) -> i32 {
        self.grid.base.gridsize
    }

    pub fn insert(&mut self, id: PartId) {
        self.grid.insert_bbox(&*self.parts, true, true, id);
    }

    pub fn remove(&mut self, id: PartId) {
        self.grid.remove_bbox(&*self.parts, id);
    }

    /// `Merges`.
    pub fn merges(
        &mut self,
        box_cb: &dyn Fn(&Parts, PartId, &mut TBox) -> bool,
        confirm_cb: &dyn Fn(&Parts, &Blobs, PartId, PartId) -> bool,
    ) {
        let mut gs = GridSearch::new();
        gs.start_full_search(self.grid);
        while let Some(part) = gs.next_full_search(self.grid, &*self.parts) {
            if self.merge_part(box_cb, confirm_cb, part) {
                gs.reposition_iterator(self.grid);
            }
        }
    }

    fn merge_part(
        &mut self,
        box_cb: &dyn Fn(&Parts, PartId, &mut TBox) -> bool,
        confirm_cb: &dyn Fn(&Parts, &Blobs, PartId, PartId) -> bool,
        part: PartId,
    ) -> bool {
        if self.parts.get(part).is_unmergeable_type() {
            return false;
        }
        let mut any_done = false;
        loop {
            let mut merge_done = false;
            let mut b = self.parts.box_of(part);
            if box_cb(self.parts, part, &mut b) {
                let candidates = self.find_merge_candidates(part, &b);
                let (neighbour, overlap_increase) =
                    self.best_merge_candidate(part, &candidates, Some(confirm_cb));
                if let Some(n) = neighbour
                    && overlap_increase <= 0
                {
                    self.remove(n);
                    self.remove(part);
                    self.parts.absorb(self.blobs, part, n);
                    self.insert(part);
                    merge_done = true;
                    any_done = true;
                }
            }
            if !merge_done {
                break;
            }
        }
        any_done
    }

    fn find_merge_candidates(&self, part: PartId, search_box: &TBox) -> EList<PartId> {
        let ok_overlap =
            (TINY_ENOUGH_TEXTLINE_OVERLAP_FRACTION * f64::from(self.gridsize()) + 0.5) as i32;
        let part_box = self.parts.box_of(part);
        let mut candidates = EList::new();
        let mut rs = GridSearch::new();
        rs.set_unique_mode(true);
        rs.start_rect_search(self.grid, search_box);
        while let Some(candidate) = rs.next_rect_search(self.grid, &*self.parts) {
            if !ok_merge_candidate(self.parts, self.blobs, part, candidate) {
                continue;
            }
            let c_box = self.parts.box_of(candidate);
            if !part_box.contains(&c_box) && !c_box.contains(&part_box) {
                let mut merged = part_box;
                merged.union_with(&c_box);
                let mut ms = GridSearch::new();
                ms.set_unique_mode(true);
                ms.start_rect_search(self.grid, &merged);
                let mut blocked = false;
                while let Some(n) = ms.next_rect_search(self.grid, &*self.parts) {
                    if n == part || n == candidate {
                        continue;
                    }
                    if self.parts.ok_merge_overlap(n, part, candidate, ok_overlap) {
                        continue;
                    }
                    let n_box = self.parts.box_of(n);
                    if !n_box.overlap(&part_box)
                        && !n_box.overlap(&c_box)
                        && !ok_merge_candidate(self.parts, self.blobs, part, n)
                        && !ok_merge_candidate(self.parts, self.blobs, candidate, n)
                    {
                        blocked = true;
                        break;
                    }
                }
                if blocked {
                    continue;
                }
            }
            clist_add_sorted(&mut candidates, part_left_cmp(self.parts), true, candidate);
        }
        candidates
    }

    fn find_overlapping_partitions(&self, b: &TBox, not_this: PartId) -> EList<PartId> {
        let mut out = EList::new();
        let mut rs = GridSearch::new();
        rs.start_rect_search(self.grid, b);
        while let Some(p) = rs.next_rect_search(self.grid, &*self.parts) {
            if p != not_this {
                clist_add_sorted(&mut out, part_left_cmp(self.parts), true, p);
            }
        }
        out
    }

    /// `BestMergeCandidate`: (candidate, overlap_increase).
    fn best_merge_candidate(
        &self,
        part: PartId,
        candidates: &EList<PartId>,
        confirm_cb: Option<ConfirmCb<'_>>,
    ) -> (Option<PartId>, i32) {
        if candidates.is_empty() {
            return (None, 0);
        }
        let ok_overlap =
            (TINY_ENOUGH_TEXTLINE_OVERLAP_FRACTION * f64::from(self.gridsize()) + 0.5) as i32;
        let part_box = self.parts.box_of(part);
        let mut full_box = part_box;
        let cands = candidates.to_vec();
        for &c in &cands {
            full_box.union_with(&self.parts.box_of(c));
        }
        let neighbours = self.find_overlapping_partitions(&full_box, part);
        let non_candidate_neighbours = set_subtract(self.parts, &neighbours, candidates);
        let mut worst_nc_increase = 0;
        let mut best_increase = i32::MAX;
        let mut best_area = 0;
        let mut best: Option<PartId> = None;
        for &c in &cands {
            if let Some(cb) = confirm_cb
                && !cb(self.parts, self.blobs, part, c)
            {
                continue;
            }
            let increase = increase_in_overlap(self.parts, part, c, ok_overlap, &neighbours);
            let cand_box = self.parts.box_of(c);
            if best.is_none() || increase < best_increase {
                best = Some(c);
                best_increase = increase;
                best_area = cand_box.bounding_union(&part_box).area() - cand_box.area();
            } else if increase == best_increase {
                let area = cand_box.bounding_union(&part_box).area() - cand_box.area();
                if area < best_area {
                    best_area = area;
                    best = Some(c);
                }
            }
            let increase =
                increase_in_overlap(self.parts, part, c, ok_overlap, &non_candidate_neighbours);
            if increase > worst_nc_increase {
                worst_nc_increase = increase;
            }
        }
        if best_increase > 0
            && worst_nc_increase < best_increase
            && test_compatible_candidates(self.parts, self.blobs, part, candidates)
        {
            best_increase = worst_nc_increase;
        }
        (best, best_increase)
    }

    /// `ComputeTotalOverlap`; with `want_grid`, also builds the grid of
    /// overlapping partitions (as shallow copies).
    pub fn compute_total_overlap(&mut self, want_grid: bool) -> (i32, Option<PartGrid>) {
        let mut total = 0;
        let mut overlap_grid: Option<PartGrid> = None;
        let mut gs = GridSearch::new();
        gs.start_full_search(self.grid);
        while let Some(part) = gs.next_full_search(self.grid, &*self.parts) {
            let part_box = self.parts.box_of(part);
            let neighbours = self.find_overlapping_partitions(&part_box, part);
            let mut any_part_overlap = false;
            for n in neighbours.to_vec() {
                let n_box = self.parts.box_of(n);
                let overlap = n_box.intersection(&part_box).area();
                if overlap > 0 && want_grid {
                    let g = overlap_grid.get_or_insert_with(|| {
                        BBGrid::new(
                            self.grid.base.gridsize,
                            self.grid.base.bleft,
                            self.grid.base.tright,
                        )
                    });
                    let c = self.parts.shallow_copy(n);
                    g.insert_bbox(&*self.parts, true, true, c);
                    if !any_part_overlap {
                        let c = self.parts.shallow_copy(part);
                        g.insert_bbox(&*self.parts, true, true, c);
                    }
                }
                any_part_overlap = true;
                total += overlap;
            }
        }
        (total, overlap_grid)
    }

    /// `SplitOverlappingPartitions`.
    pub fn split_overlapping_partitions(&mut self, big_parts: &mut EList<PartId>) {
        let ok_overlap =
            (TINY_ENOUGH_TEXTLINE_OVERLAP_FRACTION * f64::from(self.gridsize()) + 0.5) as i32;
        let mut gs = GridSearch::new();
        gs.start_full_search(self.grid);
        while let Some(part) = gs.next_full_search(self.grid, &*self.parts) {
            let b = self.parts.box_of(part);
            let mut rs = GridSearch::new();
            rs.set_unique_mode(true);
            rs.start_rect_search(self.grid, &b);
            let mut unresolved = 0;
            while let Some(n) = rs.next_rect_search(self.grid, &*self.parts) {
                if n == part {
                    continue;
                }
                let n_box = self.parts.box_of(n);
                if self.parts.ok_merge_overlap(n, part, part, ok_overlap)
                    && self.parts.ok_merge_overlap(part, n, n, ok_overlap)
                {
                    continue;
                }
                if !self.parts.get(part).is_singleton() {
                    let excluded = self
                        .parts
                        .biggest_box(self.blobs, part)
                        .expect("non-empty part");
                    let shrunken = self.parts.bounds_without_box(self.blobs, part, excluded);
                    if !shrunken.overlap(&n_box)
                        && f64::from(self.blobs.get(excluded).bbox.height())
                            > BIG_PART_SIZE_RATIO * f64::from(shrunken.height())
                    {
                        gs.remove_bbox(self.grid, &*self.parts);
                        self.parts.remove_box(self.blobs, part, excluded);
                        self.parts
                            .make_big_partition(self.blobs, excluded, Some(big_parts));
                        self.insert(part);
                        gs.reposition_iterator(self.grid);
                        break;
                    }
                } else if b.contains(&n_box) {
                    unresolved += 1;
                    continue;
                }
                if !self.parts.get(n).is_singleton() {
                    let excluded = self
                        .parts
                        .biggest_box(self.blobs, n)
                        .expect("non-empty part");
                    let shrunken = self.parts.bounds_without_box(self.blobs, n, excluded);
                    if !shrunken.overlap(&b)
                        && f64::from(self.blobs.get(excluded).bbox.height())
                            > BIG_PART_SIZE_RATIO * f64::from(shrunken.height())
                    {
                        rs.remove_bbox(self.grid, &*self.parts);
                        self.parts.remove_box(self.blobs, n, excluded);
                        self.parts
                            .make_big_partition(self.blobs, excluded, Some(big_parts));
                        self.insert(n);
                        gs.reposition_iterator(self.grid);
                        break;
                    }
                }
                let part_overlap_count =
                    self.parts.count_overlapping_boxes(self.blobs, part, &n_box);
                let neighbour_overlap_count = self.parts.count_overlapping_boxes(self.blobs, n, &b);
                let mut right_part = None;
                if neighbour_overlap_count <= part_overlap_count
                    || self.parts.get(part).is_singleton()
                {
                    if let Some(split) = self.parts.overlap_split_blob(self.blobs, n, &b) {
                        rs.remove_bbox(self.grid, &*self.parts);
                        right_part = self.parts.split_at_blob(self.blobs, n, split);
                        self.insert(n);
                    }
                } else if let Some(split) = self.parts.overlap_split_blob(self.blobs, part, &n_box)
                {
                    gs.remove_bbox(self.grid, &*self.parts);
                    right_part = self.parts.split_at_blob(self.blobs, part, split);
                    self.insert(part);
                }
                if let Some(rp) = right_part {
                    self.insert(rp);
                    gs.reposition_iterator(self.grid);
                    rs.reposition_iterator(self.grid);
                    break;
                }
            }
            if unresolved > 2 && self.parts.get(part).is_singleton() {
                self.remove(part);
                self.parts.get_mut(part).block_owned = true;
                let mut it = Iter::new(big_parts);
                it.add_to_end(big_parts, part);
                gs.reposition_iterator(self.grid);
            }
        }
    }

    /// `GridSmoothNeighbours`.
    pub fn grid_smooth_neighbours(
        &mut self,
        source_type: FlowType,
        nontext_map: &Bitmap,
        im_box: &TBox,
    ) -> bool {
        let mut gs = GridSearch::new();
        gs.start_full_search(self.grid);
        let mut any_changed = false;
        while let Some(part) = gs.next_full_search(self.grid, &*self.parts) {
            let p = self.parts.get(part);
            if p.flow != source_type || is_line_type(p.blob_type) {
                continue;
            }
            if self.smooth_region_type(nontext_map, im_box, part) {
                any_changed = true;
            }
        }
        any_changed
    }

    fn smooth_region_type(&mut self, nontext_map: &Bitmap, im_box: &TBox, part: PartId) -> bool {
        let part_box = self.parts.box_of(part);
        let mut best_type = RegionType::Unknown;
        let mut best_dist = i32::MAX;
        let mut max_dist = part_box.width().min(part_box.height());
        max_dist = (max_dist * MAX_NEIGHBOUR_DIST_FACTOR).max(self.gridsize() * 2);
        let mut any_image = false;
        let mut all_image = true;
        for d in 0..4 {
            let (t, dist) = self.smooth_in_one_direction(d, nontext_map, im_box, part);
            if t != RegionType::Unknown && dist < best_dist {
                best_dist = dist;
                best_type = t;
            }
            if t == RegionType::PolyImage {
                any_image = true;
            } else {
                all_image = false;
            }
        }
        if best_dist > max_dist {
            return false;
        }
        let p = self.parts.get(part);
        if p.flow == FlowType::StrongChain && !all_image {
            return false;
        }
        let mut new_type = p.blob_type;
        let mut new_flow = p.flow;
        if best_type == RegionType::Text && !any_image {
            new_flow = FlowType::StrongChain;
            new_type = RegionType::Text;
        } else if best_type == RegionType::VertText && !any_image {
            new_flow = FlowType::StrongChain;
            new_type = RegionType::VertText;
        } else if best_type == RegionType::PolyImage {
            new_flow = FlowType::NonText;
            new_type = RegionType::Unknown;
        }
        if new_type != p.blob_type || new_flow != p.flow {
            let pm = self.parts.get_mut(part);
            pm.flow = new_flow;
            pm.blob_type = new_type;
            self.parts.set_blob_types(self.blobs, part);
            true
        } else {
            false
        }
    }

    fn smooth_in_one_direction(
        &self,
        dir: usize,
        nontext_map: &Bitmap,
        im_box: &TBox,
        part: PartId,
    ) -> (RegionType, i32) {
        let part_box = self.parts.box_of(part);
        let (search_box, scaling) = compute_search_box_and_scaling(dir, &part_box, self.gridsize());
        let image_region = count_pixels_in_box(search_box, im_box, nontext_map) > 0;
        let dists = self.accumulate_part_distances(part, scaling, &search_box, nontext_map, im_box);
        let mut counts = [0u32; NPT_COUNT];
        let image_bias = if image_region {
            SMOOTH_DECISION_MARGIN / 2
        } else {
            0
        };
        let p = self.parts.get(part);
        let (text_dir, flow_type) = (p.blob_type, p.flow);
        let mut best_distance;
        loop {
            let mut min_dist = i32::MAX;
            for i in 0..NPT_COUNT {
                if (counts[i] as usize) < dists[i].len() && dists[i][counts[i] as usize] < min_dist
                {
                    min_dist = dists[i][counts[i] as usize];
                }
            }
            for i in 0..NPT_COUNT {
                while (counts[i] as usize) < dists[i].len()
                    && dists[i][counts[i] as usize] <= min_dist
                {
                    counts[i] += 1;
                }
            }
            best_distance = min_dist;
            let image_count = counts[NPT_IMAGE];
            let htext_score = (counts[NPT_HTEXT]
                .wrapping_add(counts[NPT_WEAK_HTEXT])
                .wrapping_sub(image_count.wrapping_add(counts[NPT_WEAK_VTEXT])))
                as i32;
            let vtext_score = (counts[NPT_VTEXT]
                .wrapping_add(counts[NPT_WEAK_VTEXT])
                .wrapping_sub(image_count.wrapping_add(counts[NPT_WEAK_HTEXT])))
                as i32;
            if image_count > 0
                && image_bias - htext_score >= SMOOTH_DECISION_MARGIN
                && image_bias - vtext_score >= SMOOTH_DECISION_MARGIN
            {
                best_distance = dists[NPT_IMAGE][0];
                if !dists[NPT_WEAK_VTEXT].is_empty() && best_distance > dists[NPT_WEAK_VTEXT][0] {
                    best_distance = dists[NPT_WEAK_VTEXT][0];
                }
                if !dists[NPT_WEAK_HTEXT].is_empty() && best_distance > dists[NPT_WEAK_HTEXT][0] {
                    best_distance = dists[NPT_WEAK_HTEXT][0];
                }
                return (RegionType::PolyImage, best_distance);
            }
            if (text_dir != RegionType::VertText || flow_type != FlowType::Chain)
                && counts[NPT_HTEXT] > 0
                && htext_score >= SMOOTH_DECISION_MARGIN
            {
                return (RegionType::Text, dists[NPT_HTEXT][0]);
            } else if (text_dir != RegionType::Text || flow_type != FlowType::Chain)
                && counts[NPT_VTEXT] > 0
                && vtext_score >= SMOOTH_DECISION_MARGIN
            {
                return (RegionType::VertText, dists[NPT_VTEXT][0]);
            }
            if min_dist == i32::MAX {
                break;
            }
        }
        (RegionType::Unknown, best_distance)
    }

    fn accumulate_part_distances(
        &self,
        base: PartId,
        scaling: ICoord,
        search_box: &TBox,
        nontext_map: &Bitmap,
        im_box: &TBox,
    ) -> [Vec<i32>; NPT_COUNT] {
        let mut dists: [Vec<i32>; NPT_COUNT] = Default::default();
        let part_box = self.parts.box_of(base);
        let mut rs = GridSearch::new();
        rs.set_unique_mode(true);
        rs.start_rect_search(self.grid, search_box);
        while let Some(n) = rs.next_rect_search(self.grid, &*self.parts) {
            let np = self.parts.get(n);
            if np.is_unmergeable_type()
                || !self.parts.confirm_no_tab_violation(self.blobs, base, n)
                || n == base
            {
                continue;
            }
            let nbox = np.bounding_box;
            let n_type = np.blob_type;
            if matches!(n_type, RegionType::Text | RegionType::VertText)
                && !blank_image_in_between(&part_box, &nbox, im_box, nontext_map)
            {
                continue;
            }
            if is_line_type(n_type) {
                continue;
            }
            let x_gap = part_box.x_gap(&nbox).max(0);
            let y_gap = part_box.y_gap(&nbox).max(0);
            let n_dist = x_gap * scaling.x + y_gap * scaling.y;
            let n_boxes = np.boxes_count().min(SMOOTH_DECISION_MARGIN);
            let n_flow = np.flow;
            let idx = if n_flow == FlowType::StrongChain {
                if n_type == RegionType::Text {
                    NPT_HTEXT
                } else {
                    NPT_VTEXT
                }
            } else if matches!(n_type, RegionType::Text | RegionType::VertText)
                && matches!(n_flow, FlowType::Chain | FlowType::Neighbours)
            {
                if n_type == RegionType::Text {
                    NPT_WEAK_HTEXT
                } else {
                    NPT_WEAK_VTEXT
                }
            } else {
                NPT_IMAGE
            };
            for _ in 0..n_boxes {
                dists[idx].push(n_dist);
            }
        }
        for d in &mut dists {
            super::stdalgo::sort(d, |a, b| a < b);
        }
        dists
    }

    /// `DeleteNonLeaderParts`.
    pub fn delete_non_leader_parts(&mut self) {
        let mut gs = GridSearch::new();
        gs.start_full_search(self.grid);
        while let Some(part) = gs.next_full_search(self.grid, &*self.parts) {
            if self.parts.get(part).flow != FlowType::Leader {
                gs.remove_bbox(self.grid, &*self.parts);
                if self.parts.release_non_leader_boxes(self.blobs, part) {
                    self.insert(part);
                    gs.reposition_iterator(self.grid);
                } else {
                    self.parts.delete(part);
                }
            }
        }
    }

    /// `DeleteParts`.
    pub fn delete_parts(&mut self) {
        let mut gs = GridSearch::new();
        gs.start_full_search(self.grid);
        let mut dead = Vec::new();
        while let Some(part) = gs.next_full_search(self.grid, &*self.parts) {
            self.parts.disown_boxes(self.blobs, part);
            dead.push(part);
        }
        self.grid.clear();
        for p in dead {
            self.parts.delete(p);
        }
    }

    /// `ReTypeBlobs`.
    pub fn re_type_blobs(&mut self, im_blobs: &mut EList<BlobId>) {
        let mut im_it = Iter::new(im_blobs);
        let mut gs = GridSearch::new();
        gs.start_full_search(self.grid);
        while let Some(part) = gs.next_full_search(self.grid, &*self.parts) {
            let (blob_type, flow) = {
                let p = self.parts.get(part);
                (p.blob_type, p.flow)
            };
            let mut any_blobs_moved = false;
            if matches!(blob_type, RegionType::PolyImage | RegionType::RectImage) {
                for b in self.parts.get(part).boxes.to_vec() {
                    im_it.add_after_then_move(im_blobs, b);
                }
            } else if blob_type != RegionType::Noise {
                let p = self.parts.get_mut(part);
                let mut it = Iter::new(&p.boxes);
                it.mark_cycle_pt();
                while !it.cycled_list(&p.boxes) {
                    let b = it.data(&p.boxes);
                    let bb = self.blobs.get_mut(b);
                    if bb.region_type == RegionType::Noise {
                        bb.owner = None;
                        it.extract(&mut p.boxes);
                        any_blobs_moved = true;
                    } else {
                        bb.region_type = blob_type;
                        if bb.flow != FlowType::Leader {
                            bb.flow = flow;
                        }
                    }
                    it.forward(&p.boxes);
                }
            }
            if blob_type == RegionType::Noise || self.parts.get(part).boxes.is_empty() {
                self.parts.disown_boxes(self.blobs, part);
                gs.remove_bbox(self.grid, &*self.parts);
                for b in self.parts.get(part).boxes.to_vec() {
                    if self
                        .blobs
                        .get(b)
                        .cblob
                        .as_ref()
                        .is_some_and(|c| c.area() == 0)
                    {
                        self.blobs.delete(b);
                    }
                }
                self.parts.delete(part);
            } else if any_blobs_moved {
                gs.remove_bbox(self.grid, &*self.parts);
                self.parts.compute_limits(self.blobs, part);
                self.insert(part);
                gs.reposition_iterator(self.grid);
            }
        }
    }
}

/// `ListNeighbours` / `List2ndNeighbours`.
pub fn list_neighbours(blobs: &Blobs, blob: BlobId, out: &mut EList<BlobId>) {
    let cmp = |a: BlobId, b: BlobId| sort_by_box_left(&blobs.get(a).bbox, &blobs.get(b).bbox);
    for dir in 0..4 {
        if let Some(n) = blobs.get(blob).neighbours[dir] {
            clist_add_sorted(out, cmp, true, n);
        }
    }
}

pub fn list_2nd_neighbours(blobs: &Blobs, blob: BlobId, out: &mut EList<BlobId>) {
    list_neighbours(blobs, blob, out);
    for dir in 0..4 {
        if let Some(n) = blobs.get(blob).neighbours[dir] {
            list_neighbours(blobs, n, out);
        }
    }
}

pub const DIRS: [usize; 4] = [BND_LEFT, BND_BELOW, BND_RIGHT, BND_ABOVE];
