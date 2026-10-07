//! `ColPartition` (`colpartition.cpp`): a run of blobs forming a textline
//! candidate, as used by the sparse-text layout path. Partitions live in an
//! arena ([`Parts`]) and are referred to by id.

use super::blobbox::{BlobId, Blobs, FlowType, RegionType};
use super::elist::{EList, Iter};
use super::geom::{ICoord, TBox};
use super::grid::{BoxOf, clist_add_sorted, sort_by_box_bottom, sort_by_box_left};
use super::stats::Stats;
use super::tabvector::TabVector;

pub type PartId = u32;

/// `PolyBlockType` (the subset the layout produces).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolyBlockType {
    Unknown,
    FlowingText,
    HeadingText,
    PulloutText,
    Equation,
    InlineEquation,
    Table,
    VerticalText,
    CaptionText,
    FlowingImage,
    HeadingImage,
    PulloutImage,
    HorzLine,
    VertLine,
    Noise,
}

impl PolyBlockType {
    pub fn is_line(self) -> bool {
        matches!(self, PolyBlockType::HorzLine | PolyBlockType::VertLine)
    }

    pub fn is_image(self) -> bool {
        matches!(
            self,
            PolyBlockType::FlowingImage | PolyBlockType::HeadingImage | PolyBlockType::PulloutImage
        )
    }

    pub fn is_text(self) -> bool {
        matches!(
            self,
            PolyBlockType::FlowingText
                | PolyBlockType::HeadingText
                | PolyBlockType::PulloutText
                | PolyBlockType::Table
                | PolyBlockType::VerticalText
                | PolyBlockType::CaptionText
                | PolyBlockType::InlineEquation
        )
    }
}

const MAX_LEADER_GAP_FRACTION_OF_MAX: f64 = 0.25;
const MAX_LEADER_GAP_FRACTION_OF_MIN: f64 = 0.5;
const MIN_LEADER_COUNT: i32 = 5;
const MIN_STRONG_TEXT_VALUE: i32 = 6;
const MIN_CHAIN_TEXT_VALUE: i32 = 3;
const HORZ_STRONG_TEXTLINE_COUNT: i32 = 8;
const HORZ_STRONG_TEXTLINE_HEIGHT: i32 = 10;
const HORZ_STRONG_TEXTLINE_ASPECT: i32 = 5;

#[derive(Clone, Debug)]
pub struct ColPartition {
    pub left_margin: i32,
    pub right_margin: i32,
    pub bounding_box: TBox,
    pub median_bottom: i32,
    pub median_top: i32,
    pub median_height: i32,
    pub median_left: i32,
    pub median_right: i32,
    pub median_width: i32,
    pub blob_type: RegionType,
    pub flow: FlowType,
    pub good_blob_score: i32,
    pub good_width: bool,
    pub good_column: bool,
    pub left_key_tab: bool,
    pub right_key_tab: bool,
    pub left_key: i32,
    pub right_key: i32,
    pub ptype: PolyBlockType,
    pub vertical: ICoord,
    pub boxes: EList<BlobId>,
    pub upper_partners: EList<PartId>,
    pub lower_partners: EList<PartId>,
    pub last_add_was_vertical: bool,
    pub block_owned: bool,
    pub owns_blobs: bool,
    pub first_column: i32,
    pub last_column: i32,
    pub special_blobs_densities: [f32; 6],
    pub alive: bool,
}

impl ColPartition {
    pub fn new(blob_type: RegionType, vertical: ICoord) -> ColPartition {
        ColPartition {
            left_margin: -i32::MAX,
            right_margin: i32::MAX,
            bounding_box: TBox::default(),
            median_bottom: i32::MAX,
            median_top: -i32::MAX,
            median_height: 0,
            median_left: i32::MAX,
            median_right: -i32::MAX,
            median_width: 0,
            blob_type,
            flow: FlowType::None,
            good_blob_score: 0,
            good_width: false,
            good_column: false,
            left_key_tab: false,
            right_key_tab: false,
            left_key: 0,
            right_key: 0,
            ptype: PolyBlockType::Unknown,
            vertical,
            boxes: EList::new(),
            upper_partners: EList::new(),
            lower_partners: EList::new(),
            last_add_was_vertical: false,
            block_owned: false,
            owns_blobs: true,
            first_column: -1,
            last_column: -1,
            special_blobs_densities: [0.0; 6],
            alive: true,
        }
    }

    pub fn mid_y(&self) -> i32 {
        (self.bounding_box.top + self.bounding_box.bottom) / 2
    }

    pub fn sort_key(&self, x: i32, y: i32) -> i32 {
        TabVector::sort_key_of(self.vertical, x, y)
    }

    pub fn box_left_key(&self) -> i32 {
        self.sort_key(self.bounding_box.left, self.mid_y())
    }

    pub fn box_right_key(&self) -> i32 {
        self.sort_key(self.bounding_box.right, self.mid_y())
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    pub fn is_singleton(&self) -> bool {
        self.boxes.len() == 1
    }

    pub fn boxes_count(&self) -> i32 {
        self.boxes.len() as i32
    }

    pub fn vcore_overlap(&self, o: &ColPartition) -> i32 {
        if self.median_bottom == i32::MAX || o.median_bottom == i32::MAX {
            return 0;
        }
        self.median_top.min(o.median_top) - self.median_bottom.max(o.median_bottom)
    }

    pub fn hcore_overlap(&self, o: &ColPartition) -> i32 {
        self.median_right.min(o.median_right) - self.median_left.max(o.median_left)
    }

    pub fn vsignificant_core_overlap(&self, o: &ColPartition) -> bool {
        if self.median_bottom == i32::MAX || o.median_bottom == i32::MAX {
            return false;
        }
        let overlap = self.vcore_overlap(o);
        let height = (self.median_top - self.median_bottom).min(o.median_top - o.median_bottom);
        overlap * 3 > height
    }

    pub fn types_match(t1: RegionType, t2: RegionType) -> bool {
        (t1 == t2 || t1 == RegionType::Unknown || t2 == RegionType::Unknown)
            && !is_line_type(t1)
            && !is_line_type(t2)
    }

    pub fn is_vertical_type(&self) -> bool {
        matches!(self.blob_type, RegionType::VertText | RegionType::VLine)
    }

    pub fn is_horizontal_type(&self) -> bool {
        matches!(self.blob_type, RegionType::Text | RegionType::HLine)
    }

    pub fn is_image_ptype(&self) -> bool {
        self.ptype.is_image()
    }

    pub fn is_unmergeable_type(&self) -> bool {
        unmergeable(self.blob_type) || self.ptype == PolyBlockType::Noise
    }
}

pub fn is_line_type(t: RegionType) -> bool {
    matches!(t, RegionType::HLine | RegionType::VLine)
}

pub fn is_text_type(t: RegionType) -> bool {
    matches!(t, RegionType::Text | RegionType::VertText)
}

pub fn unmergeable(t: RegionType) -> bool {
    is_line_type(t) || matches!(t, RegionType::RectImage | RegionType::PolyImage)
}

/// `DominatesInMerge`.
pub fn dominates_in_merge(t1: FlowType, t2: FlowType) -> bool {
    if t1 == FlowType::Leader {
        return false;
    }
    if t2 == FlowType::Leader {
        return true;
    }
    t1 >= t2
}

/// Arena of partitions.
#[derive(Default, Debug)]
pub struct Parts {
    pub v: Vec<ColPartition>,
}

impl BoxOf<PartId> for Parts {
    fn box_of(&self, t: PartId) -> TBox {
        self.v[t as usize].bounding_box
    }
}

fn box_left_cmp(blobs: &Blobs) -> impl Fn(BlobId, BlobId) -> i32 + '_ {
    move |a, b| sort_by_box_left(&blobs.get(a).bbox, &blobs.get(b).bbox)
}

fn box_bottom_cmp(blobs: &Blobs) -> impl Fn(BlobId, BlobId) -> i32 + '_ {
    move |a, b| sort_by_box_bottom(&blobs.get(a).bbox, &blobs.get(b).bbox)
}

impl Parts {
    pub fn add(&mut self, p: ColPartition) -> PartId {
        self.v.push(p);
        (self.v.len() - 1) as PartId
    }

    pub fn get(&self, id: PartId) -> &ColPartition {
        let p = &self.v[id as usize];
        debug_assert!(p.alive, "use of deleted partition");
        p
    }

    pub fn get_mut(&mut self, id: PartId) -> &mut ColPartition {
        &mut self.v[id as usize]
    }

    /// `delete part`.
    pub fn delete(&mut self, id: PartId) {
        let p = &mut self.v[id as usize];
        p.alive = false;
        p.boxes = EList::new();
    }

    /// `AddBox`.
    pub fn add_box(&mut self, blobs: &Blobs, id: PartId, b: BlobId) {
        let bx = blobs.get(b).bbox;
        let p = self.get_mut(id);
        if p.boxes.is_empty() {
            p.bounding_box = bx;
        } else {
            p.bounding_box.union_with(&bx);
        }
        if p.is_vertical_type() {
            if !p.last_add_was_vertical {
                p.boxes.sort_by(|x, y| {
                    sort_by_box_bottom(&blobs.get(*x).bbox, &blobs.get(*y).bbox).cmp(&0)
                });
                p.last_add_was_vertical = true;
            }
            clist_add_sorted(&mut p.boxes, box_bottom_cmp(blobs), true, b);
        } else {
            if p.last_add_was_vertical {
                p.boxes.sort_by(|x, y| {
                    sort_by_box_left(&blobs.get(*x).bbox, &blobs.get(*y).bbox).cmp(&0)
                });
                p.last_add_was_vertical = false;
            }
            clist_add_sorted(&mut p.boxes, box_left_cmp(blobs), true, b);
        }
        if !p.left_key_tab {
            p.left_key = p.box_left_key();
        }
        if !p.right_key_tab {
            p.right_key = p.box_right_key();
        }
    }

    /// `RemoveBox`.
    pub fn remove_box(&mut self, blobs: &Blobs, id: PartId, b: BlobId) {
        let p = self.get_mut(id);
        let mut it = Iter::new(&p.boxes);
        it.mark_cycle_pt();
        while !it.cycled_list(&p.boxes) {
            if it.data(&p.boxes) == b {
                it.extract(&mut p.boxes);
                self.compute_limits(blobs, id);
                return;
            }
            it.forward(&p.boxes);
        }
    }

    pub fn biggest_box(&self, blobs: &Blobs, id: PartId) -> Option<BlobId> {
        let p = self.get(id);
        let mut biggest: Option<BlobId> = None;
        for b in p.boxes.to_vec() {
            let bx = blobs.get(b).bbox;
            match biggest {
                None => biggest = Some(b),
                Some(g) => {
                    let gb = blobs.get(g).bbox;
                    if p.is_vertical_type() {
                        if bx.width() > gb.width() {
                            biggest = Some(b);
                        }
                    } else if bx.height() > gb.height() {
                        biggest = Some(b);
                    }
                }
            }
        }
        biggest
    }

    pub fn bounds_without_box(&self, blobs: &Blobs, id: PartId, b: BlobId) -> TBox {
        let mut r = TBox::default();
        for x in self.get(id).boxes.to_vec() {
            if x != b {
                r.union_with(&blobs.get(x).bbox);
            }
        }
        r
    }

    pub fn claim_boxes(&self, blobs: &mut Blobs, id: PartId) {
        for b in self.get(id).boxes.to_vec() {
            let bb = blobs.get_mut(b);
            if bb.owner.is_none() {
                bb.owner = Some(id);
            }
        }
    }

    pub fn disown_boxes(&self, blobs: &mut Blobs, id: PartId) {
        for b in self.get(id).boxes.to_vec() {
            blobs.get_mut(b).owner = None;
        }
    }

    pub fn disown_boxes_no_assert(&self, blobs: &mut Blobs, id: PartId) {
        for b in self.get(id).boxes.to_vec() {
            let bb = blobs.get_mut(b);
            if bb.owner == Some(id) {
                bb.owner = None;
            }
        }
    }

    pub fn release_non_leader_boxes(&mut self, blobs: &mut Blobs, id: PartId) -> bool {
        {
            let p = self.get_mut(id);
            let mut it = Iter::new(&p.boxes);
            it.mark_cycle_pt();
            while !it.cycled_list(&p.boxes) {
                let b = it.data(&p.boxes);
                if blobs.get(b).flow != FlowType::Leader {
                    if blobs.get(b).owner == Some(id) {
                        blobs.get_mut(b).owner = None;
                    }
                    it.extract(&mut p.boxes);
                }
                it.forward(&p.boxes);
            }
            if p.boxes.is_empty() {
                return false;
            }
            p.flow = FlowType::Leader;
        }
        self.compute_limits(blobs, id);
        true
    }

    /// `DeleteBoxes`: the blobs are destroyed.
    pub fn delete_boxes(&mut self, blobs: &mut Blobs, id: PartId) {
        for b in self.get_mut(id).boxes.take_all() {
            blobs.delete(b);
        }
    }

    pub fn left_blob_rule(&self, blobs: &Blobs, id: PartId) -> i32 {
        let b = self.get(id).boxes.to_vec()[0];
        blobs.get(b).left_rule
    }

    pub fn right_blob_rule(&self, blobs: &Blobs, id: PartId) -> i32 {
        let v = self.get(id).boxes.to_vec();
        blobs.get(v[v.len() - 1]).right_rule
    }

    /// `ConfirmNoTabViolation`.
    pub fn confirm_no_tab_violation(&self, blobs: &Blobs, a: PartId, o: PartId) -> bool {
        let (ab, ob) = (self.get(a).bounding_box, self.get(o).bounding_box);
        if ab.right < ob.left && ab.right < self.left_blob_rule(blobs, o) {
            return false;
        }
        if ob.right < ab.left && ob.right < self.left_blob_rule(blobs, a) {
            return false;
        }
        if ab.left > ob.right && ab.left > self.right_blob_rule(blobs, o) {
            return false;
        }
        if ob.left > ab.right && ob.left > self.right_blob_rule(blobs, a) {
            return false;
        }
        true
    }

    /// `OKDiacriticMerge`.
    pub fn ok_diacritic_merge(&self, blobs: &Blobs, id: PartId, candidate: PartId) -> bool {
        let mut min_top = i32::MAX;
        let mut max_bottom = -i32::MAX;
        for b in self.get(id).boxes.to_vec() {
            let bb = blobs.get(b);
            if !bb.is_diacritic() {
                return false;
            }
            min_top = min_top.min(bb.base_char_top);
            max_bottom = max_bottom.max(bb.base_char_bottom);
        }
        let c = self.get(candidate);
        min_top > c.median_bottom && max_bottom < c.median_top
    }

    /// `MakeBigPartition`.
    pub fn make_big_partition(
        &mut self,
        blobs: &mut Blobs,
        b: BlobId,
        big_parts: Option<&mut EList<PartId>>,
    ) -> PartId {
        blobs.get_mut(b).owner = None;
        let id = self.add(ColPartition::new(RegionType::Unknown, ICoord::new(0, 1)));
        self.get_mut(id).flow = FlowType::None;
        self.add_box(blobs, id, b);
        self.compute_limits(blobs, id);
        self.claim_boxes(blobs, id);
        self.set_blob_types(blobs, id);
        self.get_mut(id).block_owned = true;
        if let Some(l) = big_parts {
            let mut it = Iter::new(l);
            it.add_to_end(l, id);
        }
        id
    }

    /// `Absorb` (without a width callback). `other` is deleted.
    pub fn absorb(&mut self, blobs: &mut Blobs, id: PartId, other: PartId) {
        let o = self.get(other).clone();
        {
            let p = self.get_mut(id);
            p.special_blobs_densities = [0.0; 6];
            let w1 = p.boxes.len() as u32;
            let w2 = o.boxes.len() as u32;
            for t in 0..6 {
                let new_val = p.special_blobs_densities[t] * w1 as f32
                    + o.special_blobs_densities[t] * w2 as f32;
                if w1 == 0 || w2 == 0 {
                    p.special_blobs_densities[t] = new_val / (w1 + w2) as f32;
                }
            }
            let mut it = Iter::new(&p.boxes);
            for b in o.boxes.to_vec() {
                let prev_owner = blobs.get(b).owner;
                if prev_owner != Some(other) && prev_owner.is_some() {
                    continue;
                }
                if prev_owner == Some(other) {
                    blobs.get_mut(b).owner = Some(id);
                }
                it.add_to_end(&mut p.boxes, b);
            }
            p.left_margin = p.left_margin.min(o.left_margin);
            p.right_margin = p.right_margin.max(o.right_margin);
            if o.left_key < p.left_key {
                p.left_key = o.left_key;
                p.left_key_tab = o.left_key_tab;
            }
            if o.right_key > p.right_key {
                p.right_key = o.right_key;
                p.right_key_tab = o.right_key_tab;
            }
            if !dominates_in_merge(p.flow, o.flow) {
                p.flow = o.flow;
                p.blob_type = o.blob_type;
            }
        }
        self.get_mut(other).boxes = EList::new();
        self.set_blob_types(blobs, id);
        {
            let p = self.get_mut(id);
            if p.is_vertical_type() {
                p.boxes.sort_by(|x, y| {
                    sort_by_box_bottom(&blobs.get(*x).bbox, &blobs.get(*y).bbox).cmp(&0)
                });
                p.last_add_was_vertical = true;
            } else {
                p.boxes.sort_by(|x, y| {
                    sort_by_box_left(&blobs.get(*x).bbox, &blobs.get(*y).bbox).cmp(&0)
                });
                p.last_add_was_vertical = false;
            }
        }
        self.compute_limits(blobs, id);
        // Partner lists are empty in the sparse layout path.
        self.delete(other);
    }

    /// `OKMergeOverlap`.
    pub fn ok_merge_overlap(
        &self,
        id: PartId,
        m1: PartId,
        m2: PartId,
        ok_box_overlap: i32,
    ) -> bool {
        let p = self.get(id);
        let (a, b) = (self.get(m1), self.get(m2));
        if p.is_vertical_type() || a.is_vertical_type() || b.is_vertical_type() {
            return false;
        }
        if !a.vsignificant_core_overlap(b) {
            return false;
        }
        let mut merged = a.bounding_box;
        merged.union_with(&b.bounding_box);
        !(merged.bottom < p.median_top
            && merged.top > p.median_bottom
            && merged.bottom < p.bounding_box.top - ok_box_overlap
            && merged.top > p.bounding_box.bottom + ok_box_overlap)
    }

    /// `OverlapSplitBlob`.
    pub fn overlap_split_blob(&self, blobs: &Blobs, id: PartId, b: &TBox) -> Option<BlobId> {
        let v = self.get(id).boxes.to_vec();
        if v.len() <= 1 {
            return None;
        }
        let mut left_box = blobs.get(v[0]).bbox;
        for &x in &v[1..] {
            left_box.union_with(&blobs.get(x).bbox);
            if left_box.overlap(b) {
                return Some(x);
            }
        }
        None
    }

    /// `ShallowCopy`.
    pub fn shallow_copy(&mut self, id: PartId) -> PartId {
        let s = self.get(id);
        let mut p = ColPartition::new(s.blob_type, s.vertical);
        p.left_margin = s.left_margin;
        p.right_margin = s.right_margin;
        p.bounding_box = s.bounding_box;
        p.special_blobs_densities = s.special_blobs_densities;
        p.median_bottom = s.median_bottom;
        p.median_top = s.median_top;
        p.median_height = s.median_height;
        p.median_left = s.median_left;
        p.median_right = s.median_right;
        p.median_width = s.median_width;
        p.good_width = s.good_width;
        p.good_column = s.good_column;
        p.left_key_tab = s.left_key_tab;
        p.right_key_tab = s.right_key_tab;
        p.ptype = s.ptype;
        p.flow = s.flow;
        p.left_key = s.left_key;
        p.right_key = s.right_key;
        p.first_column = s.first_column;
        p.last_column = s.last_column;
        p.owns_blobs = false;
        self.add(p)
    }

    /// `SplitAtBlob`.
    pub fn split_at_blob(
        &mut self,
        blobs: &mut Blobs,
        id: PartId,
        split_blob: BlobId,
    ) -> Option<PartId> {
        let split = self.shallow_copy(id);
        let owns = self.get(id).owns_blobs;
        self.get_mut(split).owns_blobs = owns;
        let mut boxes = std::mem::take(&mut self.get_mut(id).boxes);
        let mut it = Iter::new(&boxes);
        it.mark_cycle_pt();
        while !it.cycled_list(&boxes) {
            let b = it.data(&boxes);
            let prev_owner = blobs.get(b).owner;
            if b == split_blob || !self.get(split).boxes.is_empty() {
                let x = it.extract(&mut boxes);
                self.add_box(blobs, split, x);
                if owns && prev_owner.is_some() {
                    blobs.get_mut(b).owner = Some(split);
                }
            }
            it.forward(&boxes);
        }
        self.get_mut(id).boxes = boxes;
        if self.get(split).is_empty() {
            self.delete(split);
            return None;
        }
        self.get_mut(id).right_key_tab = false;
        self.get_mut(split).left_key_tab = false;
        self.compute_limits(blobs, id);
        self.compute_limits(blobs, split);
        Some(split)
    }

    /// `ComputeLimits`.
    pub fn compute_limits(&mut self, blobs: &Blobs, id: PartId) {
        let p = self.get_mut(id);
        p.bounding_box = TBox::default();
        let ids = p.boxes.to_vec();
        let mut non_leader_count = 0;
        if ids.is_empty() {
            p.bounding_box.left = p.left_margin;
            p.bounding_box.right = p.right_margin;
            p.bounding_box.bottom = 0;
            p.bounding_box.top = 0;
        } else {
            for &b in &ids {
                let bb = blobs.get(b);
                p.bounding_box.union_with(&bb.bbox);
                if bb.flow != FlowType::Leader {
                    non_leader_count += 1;
                }
            }
        }
        if !p.left_key_tab {
            p.left_key = p.box_left_key();
        }
        if !p.right_key_tab {
            p.right_key = p.box_right_key();
        }
        if ids.is_empty() {
            return;
        }
        let bb = p.bounding_box;
        if p.is_image_ptype()
            || matches!(p.blob_type, RegionType::RectImage | RegionType::PolyImage)
        {
            p.median_top = bb.top;
            p.median_bottom = bb.bottom;
            p.median_height = bb.height();
            p.median_left = bb.left;
            p.median_right = bb.right;
            p.median_width = bb.width();
        } else {
            let mut top = Stats::new(bb.bottom, bb.top);
            let mut bottom = Stats::new(bb.bottom, bb.top);
            let mut height = Stats::new(0, bb.height());
            let mut left = Stats::new(bb.left, bb.right);
            let mut right = Stats::new(bb.left, bb.right);
            let mut width = Stats::new(0, bb.width());
            for &b in &ids {
                let x = blobs.get(b);
                if non_leader_count == 0 || x.flow != FlowType::Leader {
                    let bx = x.bbox;
                    let area = bx.area();
                    top.add(bx.top, area);
                    bottom.add(bx.bottom, area);
                    height.add(bx.height(), area);
                    left.add(bx.left, area);
                    right.add(bx.right, area);
                    width.add(bx.width(), area);
                }
            }
            p.median_top = (top.median() + 0.5) as i32;
            p.median_bottom = (bottom.median() + 0.5) as i32;
            p.median_height = (height.median() + 0.5) as i32;
            p.median_left = (left.median() + 0.5) as i32;
            p.median_right = (right.median() + 0.5) as i32;
            p.median_width = (width.median() + 0.5) as i32;
        }
    }

    pub fn count_overlapping_boxes(&self, blobs: &Blobs, id: PartId, b: &TBox) -> i32 {
        self.get(id)
            .boxes
            .to_vec()
            .iter()
            .filter(|&&x| b.overlap(&blobs.get(x).bbox))
            .count() as i32
    }

    /// `MarkAsLeaderIfMonospaced`.
    pub fn mark_as_leader_if_monospaced(&mut self, blobs: &mut Blobs, id: PartId) -> bool {
        let mut result = false;
        let bb = self.get(id).bounding_box;
        let mut part_width = bb.width();
        let mut gap_stats = Stats::new(0, part_width - 1);
        let mut width_stats = Stats::new(0, part_width - 1);
        let ids = self.get(id).boxes.to_vec();
        let mut prev = ids[0];
        blobs.get_mut(prev).flow = FlowType::Neighbours;
        width_stats.add(blobs.get(prev).bbox.width(), 1);
        let mut blob_count = 1;
        for &b in &ids[1..] {
            let bx = blobs.get(b).bbox;
            gap_stats.add(bx.left - blobs.get(prev).bbox.right, 1);
            width_stats.add(bx.right - bx.left, 1);
            blobs.get_mut(b).flow = FlowType::Neighbours;
            prev = b;
            blob_count += 1;
        }
        let median_gap = gap_stats.median();
        let median_width = width_stats.median();
        let max_width = median_gap.max(median_width);
        let min_width = median_gap.min(median_width);
        let gap_iqr = gap_stats.ile(f64::from(0.75f32)) - gap_stats.ile(f64::from(0.25f32));
        if gap_iqr < max_width * MAX_LEADER_GAP_FRACTION_OF_MAX
            && gap_iqr < min_width * MAX_LEADER_GAP_FRACTION_OF_MIN
            && blob_count >= MIN_LEADER_COUNT
        {
            let offset = (gap_iqr * 2.0).ceil() as i32;
            let mut min_step = (median_gap + median_width + 0.5) as i32;
            let max_step = min_step + offset;
            min_step -= offset;
            let part_left = bb.left - min_step / 2;
            part_width += min_step;
            let mut projection = vec![DpPoint::default(); part_width.max(0) as usize];
            for &b in &ids {
                let bx = blobs.get(b).bbox;
                let height = bx.height();
                // The original adds to the same element for every x.
                for _x in bx.left..bx.right {
                    projection[(bx.left - part_left) as usize].local_cost += height;
                }
            }
            let best_end = DpPoint::solve(min_step, max_step, part_width, &mut projection);
            if let Some(be) = best_end
                && projection[be].total_cost < blob_count
            {
                result = true;
                let mut modified = false;
                let p = self.get_mut(id);
                let mut it = Iter::new(&p.boxes);
                it.mark_cycle_pt();
                while !it.cycled_list(&p.boxes) {
                    let b = it.data(&p.boxes);
                    let bx = blobs.get(b).bbox;
                    if it.at_first(&p.boxes) {
                        let nb = blobs.get(it.data_relative(&p.boxes, 1)).bbox;
                        let gap = nb.left - bx.right;
                        if bx.width() + gap > max_step {
                            it.extract(&mut p.boxes);
                            modified = true;
                            it.forward(&p.boxes);
                            continue;
                        }
                    }
                    if it.at_last(&p.boxes) {
                        let pb = blobs.get(it.data_relative(&p.boxes, -1)).bbox;
                        let gap = bx.left - pb.right;
                        if bx.width() + gap > max_step {
                            it.extract(&mut p.boxes);
                            modified = true;
                            break;
                        }
                    }
                    let bm = blobs.get_mut(b);
                    bm.region_type = RegionType::Text;
                    bm.flow = FlowType::Leader;
                    it.forward(&p.boxes);
                }
                if modified {
                    self.compute_limits(blobs, id);
                }
                let p = self.get_mut(id);
                p.blob_type = RegionType::Text;
                p.flow = FlowType::Leader;
            }
        }
        result
    }

    /// `SetRegionAndFlowTypesFromProjectionValue`.
    pub fn set_region_and_flow_types_from_projection_value(
        &mut self,
        blobs: &mut Blobs,
        id: PartId,
        value: i32,
    ) {
        let mut blob_count = 0;
        let mut noisy_count = 0;
        let mut hline_count = 0;
        let mut vline_count = 0;
        for b in self.get(id).boxes.to_vec() {
            blob_count += 1;
            noisy_count += blobs.noisy_neighbours(b);
            let rt = blobs.get(b).region_type;
            if rt == RegionType::HLine {
                hline_count += 1;
            }
            if rt == RegionType::VLine {
                vline_count += 1;
            }
        }
        let p = self.get_mut(id);
        p.flow = FlowType::Neighbours;
        p.blob_type = RegionType::Unknown;
        if hline_count > vline_count {
            p.flow = FlowType::None;
            p.blob_type = RegionType::HLine;
        } else if vline_count > hline_count {
            p.flow = FlowType::None;
            p.blob_type = RegionType::VLine;
        } else if !(-1..=1).contains(&value) {
            let (long_side, short_side);
            if value > 0 {
                long_side = p.bounding_box.width();
                short_side = p.bounding_box.height();
                p.blob_type = RegionType::Text;
            } else {
                long_side = p.bounding_box.height();
                short_side = p.bounding_box.width();
                p.blob_type = RegionType::VertText;
            }
            let mut strong_score = if blob_count >= HORZ_STRONG_TEXTLINE_COUNT {
                1
            } else {
                0
            };
            if short_side > HORZ_STRONG_TEXTLINE_HEIGHT {
                strong_score += 1;
            }
            if short_side * HORZ_STRONG_TEXTLINE_ASPECT < long_side {
                strong_score += 1;
            }
            p.flow = if value.abs() >= MIN_STRONG_TEXT_VALUE {
                FlowType::StrongChain
            } else if value.abs() >= MIN_CHAIN_TEXT_VALUE {
                FlowType::Chain
            } else {
                FlowType::Neighbours
            };
            if p.flow == FlowType::Chain && strong_score == 3 {
                p.flow = FlowType::StrongChain;
            }
            if p.flow == FlowType::StrongChain && value < 0 && strong_score < 2 {
                p.flow = FlowType::Chain;
            }
        }
        if p.flow == FlowType::Neighbours && noisy_count >= blob_count {
            p.flow = FlowType::NonText;
            p.blob_type = RegionType::Noise;
        }
        self.set_blob_types(blobs, id);
    }

    /// `SetBlobTypes`.
    pub fn set_blob_types(&self, blobs: &mut Blobs, id: PartId) {
        let p = self.get(id);
        if !p.owns_blobs {
            return;
        }
        for b in p.boxes.to_vec() {
            let bb = blobs.get_mut(b);
            if bb.flow != FlowType::Leader {
                bb.flow = p.flow;
            }
            bb.region_type = p.blob_type;
        }
    }
}

/// `DPPoint`: dynamic-programming points for leader detection.
#[derive(Clone, Copy, Debug)]
pub struct DpPoint {
    pub local_cost: i32,
    pub total_cost: i32,
    total_steps: i32,
    best_prev: Option<usize>,
    n: i32,
    sig_x: i32,
    sig_xsq: i64,
}

impl Default for DpPoint {
    fn default() -> Self {
        DpPoint {
            local_cost: 0,
            total_cost: i32::MAX,
            total_steps: 1,
            best_prev: None,
            n: 0,
            sig_x: 0,
            sig_xsq: 0,
        }
    }
}

impl DpPoint {
    fn update_if_better(
        &mut self,
        cost: i64,
        steps: i32,
        prev: Option<usize>,
        n: i32,
        sig_x: i32,
        sig_xsq: i64,
    ) {
        if cost < i64::from(self.total_cost) {
            self.total_cost = cost as i32;
            self.total_steps = steps;
            self.best_prev = prev;
            self.n = n;
            self.sig_x = sig_x;
            self.sig_xsq = sig_xsq;
        }
    }

    /// `CostWithVariance` of point `i` reached from `prev`.
    fn cost_with_variance(points: &mut [DpPoint], i: usize, prev: Option<usize>) -> i64 {
        let Some(pi) = prev.filter(|&p| p != i) else {
            points[i].update_if_better(0, 1, None, 0, 0, 0);
            return 0;
        };
        let p = points[pi];
        let delta = (i - pi) as i32;
        let n = p.n + 1;
        let sig_x = p.sig_x + delta;
        let sig_xsq = p.sig_xsq + i64::from(delta) * i64::from(delta);
        let mut cost = (sig_xsq - i64::from(sig_x.wrapping_mul(sig_x) / n)) / i64::from(n);
        cost += i64::from(p.total_cost);
        points[i].update_if_better(cost, p.total_steps + 1, Some(pi), n, sig_x, sig_xsq);
        cost
    }

    /// `DPPoint::Solve` with `CostWithVariance`; returns the index of the
    /// best end point.
    pub fn solve(min_step: i32, max_step: i32, size: i32, points: &mut [DpPoint]) -> Option<usize> {
        if size <= 0 || max_step < min_step || min_step >= size {
            return None;
        }
        for i in 0..size as usize {
            for offset in min_step..=max_step {
                let prev = if offset as usize <= i {
                    Some(i - offset as usize)
                } else {
                    None
                };
                let new_cost = Self::cost_with_variance(points, i, prev);
                if points[i].best_prev.is_some()
                    && offset > min_step * 2
                    && new_cost > i64::from(points[i].total_cost)
                {
                    break;
                }
            }
            points[i].total_cost = points[i].total_cost.wrapping_add(points[i].local_cost);
        }
        let size = size as usize;
        let mut best_cost = points[size - 1].total_cost;
        let mut best_end = size - 1;
        let mut end = best_end as isize - 1;
        while end >= size as isize - min_step as isize {
            let cost = points[end as usize].total_cost;
            if cost < best_cost {
                best_cost = cost;
                best_end = end as usize;
            }
            end -= 1;
        }
        Some(best_end)
    }
}
