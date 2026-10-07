//! `AlignedBlob`: searching a blob grid for vertically aligned blob edges
//! (`alignedblob.cpp`).

use super::blobbox::{BlobId, Blobs, TabType};
use super::elist::{EList, Iter};
use super::geom::ICoord;
use super::grid::{BBGrid, BoxOf, GridSearch};
use super::tabvector::{TabAlignment, TabVectors, VecId};

const ALIGNED_FRACTION: f64 = 0.03125;
const RAGGED_FRACTION: f64 = 2.5;
const ALIGNED_GAP_FRACTION: f64 = 0.75;
const RAGGED_GAP_FRACTION: f64 = 1.0;
const VLINE_ALIGNMENT: i32 = 3;
const VLINE_GUTTER: i32 = 1;
const VLINE_SEARCH_SIZE: i32 = 150;
const MIN_RAGGED_TABS: i32 = 5;
const MIN_ALIGNED_TABS: i32 = 4;
const VLINE_MIN_LENGTH: i32 = 300;
const MIN_TAB_GRADIENT: f64 = 4.0;
const MAX_SKEW_FACTOR: i32 = 15;

impl BoxOf<BlobId> for Blobs {
    fn box_of(&self, t: BlobId) -> super::geom::TBox {
        self.get(t).bbox
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AlignedBlobParams {
    pub gutter_fraction: f64,
    pub right_tab: bool,
    pub ragged: bool,
    pub alignment: TabAlignment,
    pub confirmed_type: TabType,
    pub max_v_gap: i32,
    pub min_gutter: i32,
    pub l_align_tolerance: i32,
    pub r_align_tolerance: i32,
    pub min_points: i32,
    pub min_length: i32,
    pub vertical: ICoord,
}

fn shrink_vertical(vx: i32, vy: i32) -> ICoord {
    let mut factor = 1;
    if vy > i32::from(i16::MAX) {
        factor = vy / i32::from(i16::MAX) + 1;
    }
    ICoord::new(vx / factor, vy / factor)
}

impl AlignedBlobParams {
    pub fn new(
        vertical_x: i32,
        vertical_y: i32,
        height: i32,
        v_gap_multiple: i32,
        min_gutter_width: i32,
        resolution: i32,
        align0: TabAlignment,
    ) -> AlignedBlobParams {
        let right_tab = matches!(
            align0,
            TabAlignment::RightRagged | TabAlignment::RightAligned
        );
        let ragged = matches!(align0, TabAlignment::LeftRagged | TabAlignment::RightRagged);
        let res = f64::from(resolution);
        let (gutter_fraction, l, r, min_points) = if ragged {
            let (l, r) = if align0 == TabAlignment::RightRagged {
                (
                    (res * RAGGED_FRACTION + 0.5) as i32,
                    (res * ALIGNED_FRACTION + 0.5) as i32,
                )
            } else {
                (
                    (res * ALIGNED_FRACTION + 0.5) as i32,
                    (res * RAGGED_FRACTION + 0.5) as i32,
                )
            };
            (RAGGED_GAP_FRACTION, l, r, MIN_RAGGED_TABS)
        } else {
            let t = (res * ALIGNED_FRACTION + 0.5) as i32;
            (ALIGNED_GAP_FRACTION, t, t, MIN_ALIGNED_TABS)
        };
        let mut min_gutter = (f64::from(height) * gutter_fraction + 0.5) as i32;
        if min_gutter < min_gutter_width {
            min_gutter = min_gutter_width;
        }
        AlignedBlobParams {
            gutter_fraction,
            right_tab,
            ragged,
            alignment: align0,
            confirmed_type: TabType::Confirmed,
            max_v_gap: height * v_gap_multiple,
            min_gutter,
            l_align_tolerance: l,
            r_align_tolerance: r,
            min_points,
            min_length: 0,
            vertical: shrink_vertical(vertical_x, vertical_y),
        }
    }

    /// Parameters for vertical separator lines.
    pub fn for_vline(vertical_x: i32, vertical_y: i32, width: i32) -> AlignedBlobParams {
        AlignedBlobParams {
            gutter_fraction: 0.0,
            right_tab: false,
            ragged: false,
            alignment: TabAlignment::Separator,
            confirmed_type: TabType::VLine,
            max_v_gap: VLINE_SEARCH_SIZE,
            min_gutter: VLINE_GUTTER,
            l_align_tolerance: VLINE_ALIGNMENT.max(width),
            r_align_tolerance: VLINE_ALIGNMENT.max(width),
            min_points: 1,
            min_length: VLINE_MIN_LENGTH,
            vertical: shrink_vertical(vertical_x, vertical_y),
        }
    }
}

fn tab_type(blobs: &Blobs, b: BlobId, right: bool) -> TabType {
    let bb = blobs.get(b);
    if right {
        bb.right_tab_type
    } else {
        bb.left_tab_type
    }
}

fn set_tab_type(blobs: &mut Blobs, b: BlobId, right: bool, t: TabType) {
    let bb = blobs.get_mut(b);
    if right {
        bb.right_tab_type = t;
    } else {
        bb.left_tab_type = t;
    }
}

/// `AlignedBlob::FindVerticalAlignment`.
pub fn find_vertical_alignment(
    grid: &BBGrid<BlobId>,
    blobs: &mut Blobs,
    vectors: &mut TabVectors,
    p: &AlignedBlobParams,
    bbox: BlobId,
    vertical_x: &mut i32,
    vertical_y: &mut i32,
) -> Option<VecId> {
    let mut good_points: EList<BlobId> = EList::new();
    let mut ext_end_y = 0;
    let mut ext_start_y = 0;
    let mut pt_count = align_tabs(
        grid,
        blobs,
        p,
        false,
        bbox,
        &mut good_points,
        &mut ext_end_y,
    );
    pt_count += align_tabs(
        grid,
        blobs,
        p,
        true,
        bbox,
        &mut good_points,
        &mut ext_start_y,
    );
    let pts = good_points.to_vec();
    let last = blobs.get(*pts.last()?).bbox;
    let end_y = last.top;
    let end_x = if p.right_tab { last.right } else { last.left };
    let first = blobs.get(pts[0]).bbox;
    let start_x = if p.right_tab { first.right } else { first.left };
    let start_y = first.bottom;
    let crossings: i32 = pts.iter().map(|&b| blobs.get(b).line_crossings).sum();
    let at_least_2_crossings = crossings >= 2;
    if (pt_count >= p.min_points
        && end_y - start_y >= p.min_length
        && (p.ragged
            || f64::from(end_y - start_y) >= f64::from((end_x - start_x).abs()) * MIN_TAB_GRADIENT))
        || at_least_2_crossings
    {
        let confirmed_points = pts
            .iter()
            .filter(|&&b| tab_type(blobs, b, p.right_tab) == p.confirmed_type)
            .count() as i32;
        if !p.ragged || confirmed_points + confirmed_points < pt_count {
            for &b in &pts {
                set_tab_type(blobs, b, p.right_tab, p.confirmed_type);
            }
            let result = vectors.fit_vector(
                blobs,
                p.alignment,
                p.vertical,
                ext_start_y,
                ext_end_y,
                &good_points,
                vertical_x,
                vertical_y,
            );
            if let Some(r) = result {
                vectors.get_mut(r).intersects_other_lines = at_least_2_crossings;
            }
            return result;
        }
    }
    None
}

fn align_tabs(
    grid: &BBGrid<BlobId>,
    blobs: &mut Blobs,
    p: &AlignedBlobParams,
    top_to_bottom: bool,
    start: BlobId,
    good_points: &mut EList<BlobId>,
    end_y: &mut i32,
) -> i32 {
    let mut ptcount = 0;
    let mut it = Iter::new(good_points);
    let b = blobs.get(start).bbox;
    let mut x_start = if p.right_tab { b.right } else { b.left };
    let mut bbox = Some(start);
    while let Some(cur) = bbox {
        let t = tab_type(blobs, cur, p.right_tab);
        if ((t != TabType::None && t != TabType::MaybeRagged) || p.ragged)
            && (good_points.is_empty() || it.data(good_points) != cur)
        {
            if top_to_bottom {
                it.add_before_then_move(good_points, cur);
            } else {
                it.add_after_then_move(good_points, cur);
            }
            ptcount += 1;
        }
        bbox = find_aligned_blob(grid, blobs, p, top_to_bottom, cur, x_start, end_y);
        if let Some(n) = bbox
            && !p.ragged
        {
            let nb = blobs.get(n).bbox;
            x_start = if p.right_tab { nb.right } else { nb.left };
        }
    }
    ptcount
}

fn find_aligned_blob(
    grid: &BBGrid<BlobId>,
    blobs: &mut Blobs,
    p: &AlignedBlobParams,
    top_to_bottom: bool,
    bbox: BlobId,
    x_start: i32,
    end_y: &mut i32,
) -> Option<BlobId> {
    let b = blobs.get(bbox).bbox;
    let start_y = if top_to_bottom { b.bottom } else { b.top };
    let skew_tolerance = p.max_v_gap / MAX_SKEW_FACTOR;
    let mut x2 = (p.max_v_gap * p.vertical.x + p.vertical.y / 2) / p.vertical.y;
    if top_to_bottom {
        x2 = x_start - x2;
        *end_y = start_y - p.max_v_gap;
    } else {
        x2 += x_start;
        *end_y = start_y + p.max_v_gap;
    }
    let mut xmin = x_start.min(x2) - skew_tolerance;
    let mut xmax = x_start.max(x2) + skew_tolerance;
    if p.right_tab {
        xmax += p.min_gutter;
        xmin -= p.l_align_tolerance;
    } else {
        xmax += p.r_align_tolerance;
        xmin -= p.min_gutter;
    }
    let mut vs = GridSearch::new();
    vs.start_vertical_search(grid, xmin, xmax, start_y);
    let mut result: Option<BlobId> = None;
    let mut backup: Option<BlobId> = None;
    let gridsize = grid.base.gridsize;
    while let Some(n) = vs.next_vertical_search(grid, top_to_bottom) {
        if n == bbox {
            continue;
        }
        let nbox = blobs.get(n).bbox;
        let n_y = (nbox.top + nbox.bottom) / 2;
        if (!top_to_bottom && n_y > start_y + p.max_v_gap)
            || (top_to_bottom && n_y < start_y - p.max_v_gap)
        {
            break;
        }
        if (n_y < start_y) != top_to_bottom || nbox.y_overlap(&b) {
            continue;
        }
        if let Some(r) = result
            && blobs.get(r).bbox.y_gap(&nbox) > gridsize
        {
            return Some(r);
        }
        if let Some(bk) = backup
            && p.ragged
            && result.is_none()
            && blobs.get(bk).bbox.y_gap(&nbox) > gridsize
        {
            return Some(bk);
        }
        let x_at_n_y = x_start + (n_y - start_y) * p.vertical.x / p.vertical.y;
        let nb = blobs.get(n);
        if x_at_n_y < nb.left_crossing_rule || x_at_n_y > nb.right_crossing_rule {
            continue;
        }
        let (n_left, n_right) = (nbox.left, nbox.right);
        let n_x = if p.right_tab { n_right } else { n_left };
        let h = f64::from(nbox.height());
        if p.right_tab
            && n_left < x_at_n_y + p.min_gutter
            && n_right > x_at_n_y + p.r_align_tolerance
            && (p.ragged || f64::from(n_left) < f64::from(x_at_n_y) + p.gutter_fraction * h)
        {
            if blobs.get(bbox).right_tab_type >= TabType::MaybeAligned {
                blobs.get_mut(bbox).right_tab_type = TabType::Deleted;
            }
            *end_y = if top_to_bottom { nbox.top } else { nbox.bottom };
            return None;
        }
        if !p.right_tab
            && n_left < x_at_n_y - p.l_align_tolerance
            && n_right > x_at_n_y - p.min_gutter
            && (p.ragged || f64::from(n_right) > f64::from(x_at_n_y) - p.gutter_fraction * h)
        {
            if blobs.get(bbox).left_tab_type >= TabType::MaybeAligned {
                blobs.get_mut(bbox).left_tab_type = TabType::Deleted;
            }
            *end_y = if top_to_bottom { nbox.top } else { nbox.bottom };
            return None;
        }
        let nb = blobs.get(n);
        if (p.right_tab && nb.leader_on_right) || (!p.right_tab && nb.leader_on_left) {
            continue;
        }
        if n_x <= x_at_n_y + p.r_align_tolerance && n_x >= x_at_n_y - p.l_align_tolerance {
            let n_type = tab_type(blobs, n, p.right_tab);
            if n_type != TabType::None && (p.ragged || n_type != TabType::MaybeRagged) {
                match result {
                    None => result = Some(n),
                    Some(r) => {
                        let old = blobs.get(r).bbox;
                        let mut x_diff = if p.right_tab { old.right } else { old.left };
                        x_diff -= x_at_n_y;
                        let mut y_diff = (old.top + old.bottom) / 2 - start_y;
                        let old_dist = x_diff * x_diff + y_diff * y_diff;
                        x_diff = n_x - x_at_n_y;
                        y_diff = n_y - start_y;
                        let new_dist = x_diff * x_diff + y_diff * y_diff;
                        if new_dist < old_dist {
                            result = Some(n);
                        }
                    }
                }
            } else if let Some(bk) = backup {
                let bb = blobs.get(bk).bbox;
                if (p.right_tab && bb.right < nbox.right) || (!p.right_tab && bb.left > nbox.left) {
                    backup = Some(n);
                }
            } else {
                backup = Some(n);
            }
        }
    }
    result.or(backup)
}
