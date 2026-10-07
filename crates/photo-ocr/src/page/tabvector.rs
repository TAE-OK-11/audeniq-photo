//! `TabVector` and `TabConstraint` (`tabvector.cpp`). Vectors live in an
//! arena ([`TabVectors`]) and are referred to by id, as are the shared
//! constraint lists.

use super::blobbox::{BlobId, Blobs, FlowType, RegionType};
use super::detlinefit::{DetLineFit, int_cast_rounded};
use super::elist::{EList, Iter};
use super::geom::ICoord;
use super::grid::{BBGrid, GridSearch};
use super::stats::Stats;

pub type VecId = u32;
type ConsId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAlignment {
    LeftAligned,
    LeftRagged,
    CenterJustified,
    RightAligned,
    RightRagged,
    Separator,
}

const GUTTER_MULTIPLE: i32 = 4;
const GUTTER_TO_NEIGHBOUR_RATIO: i32 = 3;
const SIMILAR_VECTOR_DIST: i32 = 10;
const SIMILAR_RAGGED_DIST: i32 = 50;
const MAX_FILLIN_MULTIPLE: f64 = 11.0;
const MIN_GUTTER_FRACTION: f64 = 0.5;
const LINE_COUNT_RECIPROCAL: f64 = 4.0;
const MIN_ALIGNED_GUTTER: f64 = 0.25;
const MIN_RAGGED_GUTTER: f64 = 1.5;
const VERTICAL_GAP_FRACTION: f64 = 0.5;
const VERTICAL_BOX_RATIO: f64 = 0.5;

#[derive(Clone, Debug)]
pub struct TabVector {
    pub startpt: ICoord,
    pub endpt: ICoord,
    pub extended_ymin: i32,
    pub extended_ymax: i32,
    pub sort_key: i32,
    pub percent_score: i32,
    pub mean_width: i32,
    pub needs_refit: bool,
    pub needs_evaluation: bool,
    pub intersects_other_lines: bool,
    pub alignment: TabAlignment,
    pub boxes: EList<BlobId>,
    pub partners: EList<VecId>,
    top_constraints: Option<ConsId>,
    bottom_constraints: Option<ConsId>,
}

#[derive(Clone, Copy, Debug)]
struct TabConstraint {
    vector: VecId,
    is_top: bool,
    y_min: i32,
    y_max: i32,
}

/// Tab-stop geometry queries `TabVector::Evaluate` needs from `TabFind`.
pub trait GutterFinder {
    fn gutter_width_and_neighbour_gap(
        &mut self,
        tab_x: i32,
        mean_height: i32,
        max_gutter: i32,
        left: bool,
        bbox: BlobId,
    ) -> (i32, i32);
    fn gutter_width(
        &mut self,
        bottom_y: i32,
        top_y: i32,
        v: &TabVector,
        ignore_unmergeables: bool,
        max_gutter_width: i32,
    ) -> (i32, i32);
}

impl TabVector {
    fn empty(alignment: TabAlignment) -> TabVector {
        TabVector {
            startpt: ICoord::default(),
            endpt: ICoord::default(),
            extended_ymin: 0,
            extended_ymax: 0,
            sort_key: 0,
            percent_score: 0,
            mean_width: 0,
            needs_refit: false,
            needs_evaluation: false,
            intersects_other_lines: false,
            alignment,
            boxes: EList::new(),
            partners: EList::new(),
            top_constraints: None,
            bottom_constraints: None,
        }
    }

    pub fn x_at_y(&self, y: i32) -> i32 {
        let height = self.endpt.y - self.startpt.y;
        if height != 0 {
            (y - self.startpt.y) * (self.endpt.x - self.startpt.x) / height + self.startpt.x
        } else {
            self.startpt.x
        }
    }

    pub fn v_overlap(&self, o: &TabVector) -> i32 {
        o.endpt.y.min(self.endpt.y) - o.startpt.y.max(self.startpt.y)
    }

    pub fn v_overlap_y(&self, top_y: i32, bottom_y: i32) -> i32 {
        top_y.min(self.endpt.y) - bottom_y.max(self.startpt.y)
    }

    pub fn extended_overlap(&self, top_y: i32, bottom_y: i32) -> i32 {
        top_y.min(self.extended_ymax) - bottom_y.max(self.extended_ymin)
    }

    pub fn is_left_tab(&self) -> bool {
        matches!(
            self.alignment,
            TabAlignment::LeftAligned | TabAlignment::LeftRagged
        )
    }

    pub fn is_right_tab(&self) -> bool {
        matches!(
            self.alignment,
            TabAlignment::RightAligned | TabAlignment::RightRagged
        )
    }

    pub fn is_separator(&self) -> bool {
        self.alignment == TabAlignment::Separator
    }

    pub fn is_center_tab(&self) -> bool {
        self.alignment == TabAlignment::CenterJustified
    }

    pub fn is_ragged(&self) -> bool {
        matches!(
            self.alignment,
            TabAlignment::LeftRagged | TabAlignment::RightRagged
        )
    }

    pub fn is_left_of(&self, o: &TabVector) -> bool {
        self.sort_key < o.sort_key
    }

    pub fn partnerless(&self) -> bool {
        self.partners.is_empty()
    }

    pub fn box_count(&self) -> usize {
        self.boxes.len()
    }

    pub fn freeze(&mut self) {
        self.boxes = EList::new();
    }

    pub fn xy_flip(&mut self) {
        std::mem::swap(&mut self.startpt.x, &mut self.startpt.y);
        std::mem::swap(&mut self.endpt.x, &mut self.endpt.y);
    }

    pub fn reflect_in_y_axis(&mut self) {
        self.startpt.x = -self.startpt.x;
        self.endpt.x = -self.endpt.x;
        self.sort_key = -self.sort_key;
        self.alignment = match self.alignment {
            TabAlignment::LeftAligned => TabAlignment::RightAligned,
            TabAlignment::RightAligned => TabAlignment::LeftAligned,
            TabAlignment::LeftRagged => TabAlignment::RightRagged,
            TabAlignment::RightRagged => TabAlignment::LeftRagged,
            a => a,
        };
    }

    pub fn sort_key_of(vertical: ICoord, x: i32, y: i32) -> i32 {
        ICoord::new(x, y).cross(vertical)
    }

    pub fn x_at_y_key(vertical: ICoord, sort_key: i32, y: i32) -> i32 {
        if vertical.y != 0 {
            (vertical.x * y + sort_key) / vertical.y
        } else {
            sort_key
        }
    }

    pub fn set_y_start(&mut self, y: i32) {
        self.startpt.x = self.x_at_y(y);
        self.startpt.y = y;
    }

    pub fn set_y_end(&mut self, y: i32) {
        self.endpt.x = self.x_at_y(y);
        self.endpt.y = y;
    }

    pub fn shallow_copy(&self) -> TabVector {
        let mut c = TabVector::empty(self.alignment);
        c.startpt = self.startpt;
        c.endpt = self.endpt;
        c.extended_ymax = self.extended_ymax;
        c.extended_ymin = self.extended_ymin;
        c.intersects_other_lines = self.intersects_other_lines;
        c
    }

    /// `TabVector(src, alignment, vertical_skew, blob)`.
    pub fn from_blob(
        src: &TabVector,
        alignment: TabAlignment,
        vertical_skew: ICoord,
        blobs: &Blobs,
        blob: BlobId,
    ) -> TabVector {
        let mut v = TabVector::empty(alignment);
        v.extended_ymin = src.extended_ymin;
        v.extended_ymax = src.extended_ymax;
        v.needs_refit = true;
        v.needs_evaluation = true;
        v.boxes.push_back(blob);
        let b = blobs.get(blob).bbox;
        if v.is_left_tab() {
            v.startpt = ICoord::new(b.left, b.bottom);
            v.endpt = ICoord::new(b.left, b.top);
        } else {
            v.startpt = ICoord::new(b.right, b.bottom);
            v.endpt = ICoord::new(b.right, b.top);
        }
        v.sort_key = TabVector::sort_key_of(
            vertical_skew,
            (v.startpt.x + v.endpt.x) / 2,
            (v.startpt.y + v.endpt.y) / 2,
        );
        v
    }

    /// `ExtendToBox`.
    pub fn extend_to_box(&mut self, blobs: &Blobs, new_blob: BlobId) {
        let new_box = blobs.get(new_blob).bbox;
        let mut it = Iter::new(&self.boxes);
        if !self.boxes.is_empty() {
            let mut blob = it.data(&self.boxes);
            let mut b = blobs.get(blob).bbox;
            while !it.at_last(&self.boxes) && b.top <= new_box.top {
                if blob == new_blob {
                    return;
                }
                it.forward(&self.boxes);
                blob = it.data(&self.boxes);
                b = blobs.get(blob).bbox;
            }
            if b.top >= new_box.top {
                it.add_before_stay_put(&mut self.boxes, new_blob);
                self.needs_refit = true;
                return;
            }
        }
        self.needs_refit = true;
        it.add_after_stay_put(&mut self.boxes, new_blob);
    }

    /// `Fit`.
    pub fn fit(&mut self, blobs: &Blobs, mut vertical: ICoord, force_parallel: bool) -> bool {
        self.needs_refit = false;
        if self.boxes.is_empty() {
            if !force_parallel {
                return false;
            }
            let mid = ICoord::new(
                (self.startpt.x + self.endpt.x) / 2,
                (self.startpt.y + self.endpt.y) / 2,
            );
            self.sort_key = TabVector::sort_key_of(vertical, mid.x, mid.y);
            return self.startpt.y != self.endpt.y;
        }
        let ids = self.boxes.to_vec();
        if !force_parallel && !self.is_ragged() {
            let mut lp = DetLineFit::new();
            for (i, &id) in ids.iter().enumerate() {
                let b = blobs.get(id).bbox;
                let x1 = if self.is_right_tab() { b.right } else { b.left };
                lp.add(ICoord::new(x1, b.bottom));
                if i + 1 == ids.len() {
                    lp.add(ICoord::new(x1, b.top));
                }
            }
            let (_, s, e) = lp.fit();
            self.startpt = s;
            self.endpt = e;
            if self.startpt.y != self.endpt.y {
                vertical = self.endpt - self.startpt;
            }
        }
        let mut start_y = self.startpt.y;
        let mut end_y = self.endpt.y;
        self.sort_key = if self.is_left_tab() {
            i32::MAX
        } else {
            -i32::MAX
        };
        self.mean_width = 0;
        let mut width_count = 0;
        for (i, &id) in ids.iter().enumerate() {
            let b = blobs.get(id).bbox;
            self.mean_width += b.width();
            width_count += 1;
            let x1 = if self.is_right_tab() { b.right } else { b.left };
            let key = TabVector::sort_key_of(vertical, x1, b.bottom);
            if self.is_left_tab() == (key < self.sort_key) {
                self.sort_key = key;
                self.startpt = ICoord::new(x1, b.bottom);
            }
            let key = TabVector::sort_key_of(vertical, x1, b.top);
            if self.is_left_tab() == (key < self.sort_key) {
                self.sort_key = key;
                self.startpt = ICoord::new(x1, b.top);
            }
            if i == 0 {
                start_y = b.bottom;
            }
            if i + 1 == ids.len() {
                end_y = b.top;
            }
        }
        if width_count > 0 {
            self.mean_width = (self.mean_width + width_count - 1) / width_count;
        }
        self.endpt = self.startpt + vertical;
        self.needs_evaluation = true;
        if start_y != end_y {
            self.startpt = ICoord::new(
                TabVector::x_at_y_key(vertical, self.sort_key, start_y),
                start_y,
            );
            self.endpt = ICoord::new(TabVector::x_at_y_key(vertical, self.sort_key, end_y), end_y);
            return true;
        }
        false
    }

    pub fn is_a_partner(&self, other: VecId) -> bool {
        self.partners.to_vec().contains(&other)
    }

    pub fn get_single_partner(&self) -> Option<VecId> {
        let p = self.partners.to_vec();
        if p.len() == 1 { Some(p[0]) } else { None }
    }
}

/// Arena of tab vectors plus their shared constraint lists.
#[derive(Default, Debug)]
pub struct TabVectors {
    pub v: Vec<TabVector>,
    constraints: Vec<Option<EList<TabConstraint>>>,
}

impl TabVectors {
    pub fn add(&mut self, v: TabVector) -> VecId {
        self.v.push(v);
        (self.v.len() - 1) as VecId
    }

    pub fn get(&self, id: VecId) -> &TabVector {
        &self.v[id as usize]
    }

    pub fn get_mut(&mut self, id: VecId) -> &mut TabVector {
        &mut self.v[id as usize]
    }

    /// `TabVector::FitVector`.
    #[allow(clippy::too_many_arguments)]
    pub fn fit_vector(
        &mut self,
        blobs: &Blobs,
        alignment: TabAlignment,
        vertical: ICoord,
        extended_start_y: i32,
        extended_end_y: i32,
        good_points: &EList<BlobId>,
        vertical_x: &mut i32,
        vertical_y: &mut i32,
    ) -> Option<VecId> {
        let mut v = TabVector::empty(alignment);
        v.extended_ymin = extended_start_y;
        v.extended_ymax = extended_end_y;
        v.needs_refit = true;
        v.needs_evaluation = true;
        for b in good_points.to_vec() {
            v.boxes.push_back(b);
        }
        if !v.fit(blobs, vertical, false) {
            return None;
        }
        if !v.is_ragged() {
            let d = v.endpt - v.startpt;
            let weight = v.box_count() as i32;
            *vertical_x += d.x * weight;
            *vertical_y += d.y * weight;
        }
        Some(self.add(v))
    }

    // ---- TabConstraint ----

    fn new_constraint_list(&mut self, c: TabConstraint) -> ConsId {
        let mut l = EList::new();
        l.push_back(c);
        self.constraints.push(Some(l));
        (self.constraints.len() - 1) as ConsId
    }

    fn create_constraint(&mut self, id: VecId, is_top: bool) {
        let v = self.get(id);
        let c = if is_top {
            TabConstraint {
                vector: id,
                is_top,
                y_min: v.endpt.y,
                y_max: v.extended_ymax,
            }
        } else {
            TabConstraint {
                vector: id,
                is_top,
                y_max: v.startpt.y,
                y_min: v.extended_ymin,
            }
        };
        let l = self.new_constraint_list(c);
        if is_top {
            self.get_mut(id).top_constraints = Some(l);
        } else {
            self.get_mut(id).bottom_constraints = Some(l);
        }
    }

    fn get_constraints(&self, list: ConsId, y_min: &mut i32, y_max: &mut i32) {
        if let Some(l) = &self.constraints[list as usize] {
            for c in l.to_vec() {
                *y_min = (*y_min).max(c.y_min);
                *y_max = (*y_max).min(c.y_max);
            }
        }
    }

    fn compatible_constraints(&self, l1: Option<ConsId>, l2: Option<ConsId>) -> bool {
        if l1 == l2 {
            return false;
        }
        let mut y_min = -i32::MAX;
        let mut y_max = i32::MAX;
        if let Some(l) = l1 {
            self.get_constraints(l, &mut y_min, &mut y_max);
        }
        if let Some(l) = l2 {
            self.get_constraints(l, &mut y_min, &mut y_max);
        }
        y_max >= y_min
    }

    fn merge_constraints(&mut self, l1: Option<ConsId>, l2: Option<ConsId>) {
        if l1 == l2 {
            return;
        }
        let (Some(l1), Some(l2)) = (l1, l2) else {
            return;
        };
        let list2 = self.constraints[l2 as usize].take().unwrap_or_default();
        let items = list2.to_vec();
        for c in &items {
            if c.is_top {
                self.get_mut(c.vector).top_constraints = Some(l1);
            } else {
                self.get_mut(c.vector).bottom_constraints = Some(l1);
            }
        }
        let list1 = self.constraints[l1 as usize].get_or_insert_with(EList::new);
        let mut it = Iter::new(list1);
        it.add_list_before(list1, &items);
    }

    fn apply_constraint_list(&mut self, list: ConsId) {
        let mut y_min = -i32::MAX;
        let mut y_max = i32::MAX;
        self.get_constraints(list, &mut y_min, &mut y_max);
        let y = (y_min + y_max) / 2;
        let items = self.constraints[list as usize]
            .take()
            .map(|l| l.to_vec())
            .unwrap_or_default();
        for c in items {
            let v = self.get_mut(c.vector);
            if c.is_top {
                v.set_y_end(y);
                v.top_constraints = None;
            } else {
                v.set_y_start(y);
                v.bottom_constraints = None;
            }
        }
    }

    pub fn setup_constraints(&mut self, id: VecId) {
        self.create_constraint(id, false);
        self.create_constraint(id, true);
    }

    pub fn setup_partner_constraints(&mut self, id: VecId) {
        let partners = self.get(id).partners.to_vec();
        let mut prev_partner: Option<VecId> = None;
        let n = partners.len();
        for (i, &p) in partners.iter().enumerate() {
            let pv = self.get(p);
            if pv.top_constraints.is_none() || pv.bottom_constraints.is_none() {
                continue;
            }
            match prev_partner {
                None => {
                    let (a, b) = (
                        self.get(id).bottom_constraints,
                        self.get(p).bottom_constraints,
                    );
                    if self.compatible_constraints(a, b) {
                        self.merge_constraints(a, b);
                    }
                }
                Some(pp) => {
                    let (a, b) = (self.get(pp).top_constraints, self.get(p).bottom_constraints);
                    if self.compatible_constraints(a, b) {
                        self.merge_constraints(a, b);
                    }
                }
            }
            prev_partner = Some(p);
            if i + 1 == n {
                let (a, b) = (self.get(id).top_constraints, self.get(p).top_constraints);
                if self.compatible_constraints(a, b) {
                    self.merge_constraints(a, b);
                }
            }
        }
    }

    pub fn setup_partner_constraints_with(&mut self, id: VecId, partner: VecId) {
        let (a, b) = (
            self.get(id).bottom_constraints,
            self.get(partner).bottom_constraints,
        );
        if self.compatible_constraints(a, b) {
            self.merge_constraints(a, b);
        }
        let (a, b) = (
            self.get(id).top_constraints,
            self.get(partner).top_constraints,
        );
        if self.compatible_constraints(a, b) {
            self.merge_constraints(a, b);
        }
    }

    pub fn apply_constraints(&mut self, id: VecId) {
        if let Some(l) = self.get(id).top_constraints {
            self.apply_constraint_list(l);
        }
        if let Some(l) = self.get(id).bottom_constraints {
            self.apply_constraint_list(l);
        }
    }

    // ---- merging ----

    /// `TabVector::MergeSimilarTabVectors` over the list `vectors`.
    pub fn merge_similar_tab_vectors(
        &mut self,
        blobs: &Blobs,
        vertical: ICoord,
        vectors: &mut EList<VecId>,
        grid: Option<&BBGrid<BlobId>>,
    ) {
        let mut it1 = Iter::new(vectors);
        it1.mark_cycle_pt();
        while !it1.cycled_list(vectors) {
            let v1 = it1.data(vectors);
            let mut it2 = it1;
            it2.forward(vectors);
            while !it2.at_first(vectors) {
                let v2 = it2.data(vectors);
                if self.similar_to(blobs, vertical, v2, v1, grid) {
                    let ex = it1.extract(vectors);
                    self.merge_with(blobs, vertical, v2, ex);
                    break;
                }
                it2.forward(vectors);
            }
            it1.forward(vectors);
        }
    }

    /// `this->SimilarTo(vertical, other, grid)`.
    pub fn similar_to(
        &self,
        blobs: &Blobs,
        vertical: ICoord,
        this: VecId,
        other: VecId,
        grid: Option<&BBGrid<BlobId>>,
    ) -> bool {
        let (s, o) = (self.get(this), self.get(other));
        if !((s.is_right_tab() && o.is_right_tab()) || (s.is_left_tab() && o.is_left_tab())) {
            return false;
        }
        if s.extended_overlap(o.extended_ymax, o.extended_ymin) < 0 {
            return false;
        }
        let mut v_scale = vertical.y.abs();
        if v_scale == 0 {
            v_scale = 1;
        }
        if s.sort_key + SIMILAR_VECTOR_DIST * v_scale >= o.sort_key
            && s.sort_key - SIMILAR_VECTOR_DIST * v_scale <= o.sort_key
        {
            return true;
        }
        if !s.is_ragged()
            || !o.is_ragged()
            || s.sort_key + SIMILAR_RAGGED_DIST * v_scale < o.sort_key
            || s.sort_key - SIMILAR_RAGGED_DIST * v_scale > o.sort_key
        {
            return false;
        }
        let Some(grid) = grid else {
            return true;
        };
        let mover = if s.is_right_tab() && s.sort_key < o.sort_key {
            s
        } else {
            o
        };
        let top_y = mover.endpt.y;
        let bottom_y = mover.startpt.y;
        let mut left = mover.x_at_y(top_y).min(mover.x_at_y(bottom_y));
        let mut right = mover.x_at_y(top_y).max(mover.x_at_y(bottom_y));
        let shift = (s.sort_key - o.sort_key).abs() / v_scale;
        if s.is_right_tab() {
            right += shift;
        } else {
            left -= shift;
        }
        let mut vs = GridSearch::new();
        vs.start_vertical_search(grid, left, right, top_y);
        while let Some(b) = vs.next_vertical_search(grid, true) {
            let bb = blobs.get(b).bbox;
            if bb.top > bottom_y {
                return true;
            }
            if bb.bottom < top_y {
                continue;
            }
            let mut left_at = s.x_at_y(bb.bottom);
            let mut right_at = left_at;
            if s.is_right_tab() {
                right_at += shift;
            } else {
                left_at -= shift;
            }
            if right_at.min(bb.right) > left_at.max(bb.left) {
                return false;
            }
        }
        true
    }

    /// `this->MergeWith(vertical, other)`; `other` is then deleted.
    pub fn merge_with(&mut self, blobs: &Blobs, vertical: ICoord, this: VecId, other: VecId) {
        let o = self.get(other).clone();
        {
            let s = self.get_mut(this);
            s.extended_ymin = s.extended_ymin.min(o.extended_ymin);
            s.extended_ymax = s.extended_ymax.max(o.extended_ymax);
            if o.is_ragged() {
                s.alignment = o.alignment;
            }
            let mut it1 = Iter::new(&s.boxes);
            for bbox2 in o.boxes.to_vec() {
                let box2 = blobs.get(bbox2).bbox;
                let mut bbox1 = it1.data(&s.boxes);
                let mut box1 = blobs.get(bbox1).bbox;
                while box1.bottom < box2.bottom && !it1.at_last(&s.boxes) {
                    it1.forward(&s.boxes);
                    bbox1 = it1.data(&s.boxes);
                    box1 = blobs.get(bbox1).bbox;
                }
                if box1.bottom < box2.bottom {
                    it1.add_to_end(&mut s.boxes, bbox2);
                } else if bbox1 != bbox2 {
                    it1.add_before_stay_put(&mut s.boxes, bbox2);
                }
            }
            s.fit(blobs, vertical, true);
        }
        self.get_mut(other).boxes = EList::new();
        self.delete(other, Some(this));
    }

    /// `AddPartner`.
    pub fn add_partner(&mut self, this: VecId, partner: VecId) {
        if self.get(this).is_separator() || self.get(partner).is_separator() {
            return;
        }
        let s = self.get_mut(this);
        let mut it = Iter::new(&s.partners);
        if !s.partners.is_empty() {
            it.move_to_last(&s.partners);
            if it.data(&s.partners) == partner {
                return;
            }
        }
        it.add_after_then_move(&mut s.partners, partner);
    }

    /// `TabVector::Delete(replacement)`: unlinks `this` from its partners.
    pub fn delete(&mut self, this: VecId, replacement: Option<VecId>) {
        for partner in self.get(this).partners.to_vec() {
            let mut partner_replacement = replacement;
            if let Some(r) = replacement
                && self.get(partner).partners.to_vec().contains(&r)
            {
                partner_replacement = None;
            }
            {
                let pl = &mut self.get_mut(partner).partners;
                let mut it = Iter::new(pl);
                it.mark_cycle_pt();
                while !it.cycled_list(pl) {
                    if it.data(pl) == this {
                        it.extract(pl);
                        if let Some(r) = partner_replacement {
                            it.add_before_stay_put(pl, r);
                        }
                    }
                    it.forward(pl);
                }
            }
            if let Some(r) = partner_replacement {
                self.add_partner(r, partner);
            }
        }
        self.get_mut(this).partners = EList::new();
    }

    /// `FitAndEvaluateIfNeeded`.
    pub fn fit_and_evaluate_if_needed(
        &mut self,
        blobs: &Blobs,
        vertical: ICoord,
        id: VecId,
        finder: &mut impl GutterFinder,
    ) {
        if self.get(id).needs_refit {
            self.get_mut(id).fit(blobs, vertical, true);
        }
        if self.get(id).needs_evaluation {
            self.evaluate(blobs, vertical, id, finder);
        }
    }

    /// `Evaluate`.
    pub fn evaluate(
        &mut self,
        blobs: &Blobs,
        vertical: ICoord,
        id: VecId,
        finder: &mut impl GutterFinder,
    ) {
        self.get_mut(id).needs_evaluation = false;
        let length = self.get(id).endpt.y - self.get(id).startpt.y;
        if length == 0 || self.get(id).boxes.is_empty() {
            self.get_mut(id).percent_score = 0;
            return;
        }
        let ids = self.get(id).boxes.to_vec();
        let mut mean_height = 0;
        for &b in &ids {
            mean_height += blobs.get(b).bbox.height();
        }
        if !ids.is_empty() {
            mean_height /= ids.len() as i32;
        }
        let ragged = self.get(id).is_ragged();
        let left = self.get(id).is_left_tab();
        let max_gutter = if ragged {
            GUTTER_TO_NEIGHBOUR_RATIO * mean_height
        } else {
            GUTTER_MULTIPLE * mean_height
        };
        let mut gutters = Stats::new(0, max_gutter);
        let mut num_deleted = 0;
        let mut text_on_image = false;
        let mut good_length = 0;
        let mut prev_good: Option<super::geom::TBox> = None;
        {
            let mut list = std::mem::take(&mut self.v[id as usize].boxes);
            let mut it = Iter::new(&list);
            it.mark_cycle_pt();
            while !it.cycled_list(&list) {
                let bbox = it.data(&list);
                let b = blobs.get(bbox).bbox;
                let mid_y = (b.top + b.bottom) / 2;
                let tab_x = self.v[id as usize].x_at_y(mid_y);
                let (gutter_width, neighbour_gap) = finder.gutter_width_and_neighbour_gap(
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
                        self.v[id as usize].set_y_start(b.bottom);
                    }
                    prev_good = Some(b);
                    if blobs.get(bbox).flow == FlowType::TextOnImage {
                        text_on_image = true;
                    }
                } else {
                    it.extract(&mut list);
                    num_deleted += 1;
                }
                it.forward(&list);
            }
            self.v[id as usize].boxes = list;
        }
        let mut search_top = self.get(id).endpt.y;
        let mut search_bottom = self.get(id).startpt.y;
        let median_gutter = int_cast_rounded(gutters.median());
        if gutters.get_total() > 0 {
            prev_good = None;
            let mut list = std::mem::take(&mut self.v[id as usize].boxes);
            let mut it = Iter::new(&list);
            it.mark_cycle_pt();
            while !it.cycled_list(&list) {
                let bbox = it.data(&list);
                let b = blobs.get(bbox).bbox;
                let mid_y = (b.top + b.bottom) / 2;
                let tab_x = self.v[id as usize].x_at_y(mid_y);
                let (gutter_width, _) = finder.gutter_width_and_neighbour_gap(
                    tab_x,
                    mean_height,
                    max_gutter,
                    left,
                    bbox,
                );
                if f64::from(gutter_width) >= f64::from(median_gutter) * MIN_GUTTER_FRACTION {
                    if prev_good.is_none() {
                        self.v[id as usize].set_y_start(b.bottom);
                        search_bottom = b.top;
                    }
                    prev_good = Some(b);
                    search_top = b.bottom;
                } else {
                    it.extract(&mut list);
                    num_deleted += 1;
                }
                it.forward(&list);
            }
            self.v[id as usize].boxes = list;
        }
        if let Some(p) = prev_good {
            let v = self.get_mut(id);
            v.set_y_end(p.top);
            let length = v.endpt.y - v.startpt.y;
            v.percent_score = 100 * good_length / length;
            if num_deleted > 0 {
                v.needs_refit = true;
                self.fit_and_evaluate_if_needed(blobs, vertical, id, finder);
                if self.get(id).boxes.is_empty() {
                    return;
                }
            }
            if search_bottom > search_top {
                search_bottom = self.get(id).startpt.y;
                search_top = self.get(id).endpt.y;
            }
            let v = self.get(id);
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
            let snapshot = v.clone();
            let (gutter_width, _required_shift) = finder.gutter_width(
                search_bottom,
                search_top,
                &snapshot,
                text_on_image,
                max_gutter_width,
            );
            if f64::from(gutter_width) < min_gutter_width {
                let v = self.get_mut(id);
                v.boxes = EList::new();
                v.percent_score = 0;
            }
        } else {
            self.get_mut(id).percent_score = 0;
        }
    }

    /// `VerticalTextlinePartner`.
    pub fn vertical_textline_partner(&self, blobs: &Blobs, id: VecId) -> Option<VecId> {
        let s = self.get(id);
        let partner = s.get_single_partner()?;
        let p = self.get(partner);
        let boxes2 = p.boxes.to_vec();
        let mut i2 = 0usize;
        let mut num_matched = 0;
        let mut num_unmatched = 0;
        let mut total_widths = 0;
        let width = (s.startpt.x - p.startpt.x).abs();
        let mut gaps = Stats::new(0, width * 2 - 1);
        let mut prev: Option<BlobId> = None;
        for bbox in s.boxes.to_vec() {
            let b = blobs.get(bbox).bbox;
            if let Some(pb) = prev {
                gaps.add(b.bottom - blobs.get(pb).bbox.top, 1);
            }
            while i2 < boxes2.len()
                && boxes2[i2] != bbox
                && blobs.get(boxes2[i2]).bbox.bottom < b.bottom
            {
                i2 += 1;
            }
            if i2 < boxes2.len()
                && boxes2[i2] == bbox
                && blobs.get(bbox).region_type >= RegionType::Unknown
                && prev.is_none_or(|pb| blobs.get(pb).region_type >= RegionType::Unknown)
            {
                num_matched += 1;
            } else {
                num_unmatched += 1;
            }
            total_widths += b.width();
            prev = Some(bbox);
        }
        if num_unmatched + num_matched == 0 {
            return None;
        }
        let avg_width = f64::from(total_widths) / f64::from(num_unmatched + num_matched);
        let max_gap = VERTICAL_GAP_FRACTION * avg_width;
        let min_box_match = (f64::from(num_matched + num_unmatched) * VERTICAL_BOX_RATIO) as i32;
        let is_vertical =
            gaps.get_total() > 0 && num_matched >= min_box_match && gaps.median() <= max_gap;
        if is_vertical { Some(partner) } else { None }
    }
}
