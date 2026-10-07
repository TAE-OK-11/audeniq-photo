//! `LineFinder`: horizontal/vertical ruling-line candidates
//! (`linefind.cpp`, mask stage).

use super::alignedblob::{AlignedBlobParams, find_vertical_alignment};
use super::bitmap::Bitmap;
use super::blobbox::{BlobId, BlobNBox, Blobs, TabType};
use super::elist::{EList, Iter};
use super::geom::{ICoord, TBox};
use super::grid::{BBGrid, GridSearch};
use super::morph::{LBox, Op};
use super::outline::{Outline, outlines_to_blobs};
use super::tabvector::{TabVectors, VecId};

const THIN_LINE_FRACTION: i32 = 20;
const MIN_LINE_LENGTH_FRACTION: i32 = 4;
const MIN_THICK_LINE_WIDTH: i32 = 12;
const MAX_LINE_RESIDUE: i32 = 6;
const THICK_LENGTH_MULTIPLE: f64 = 0.75;
const MAX_NON_LINE_DENSITY: f64 = 0.25;

/// Line candidate masks after false-positive filtering.
pub struct LineMasks {
    pub vline: Option<Bitmap>,
    pub non_vline: Option<Bitmap>,
    pub hline: Option<Bitmap>,
    pub non_hline: Option<Bitmap>,
    pub intersections: Option<Bitmap>,
}

fn max_stroke_width(pix: &Bitmap) -> i32 {
    pix.max_distance_4() * 2
}

fn num_touching_intersections(b: &LBox, inter: Option<&Bitmap>) -> usize {
    let Some(inter) = inter else { return 0 };
    match inter.clip(b) {
        Some(r) => r.conn_comp(8, false).0.len(),
        None => 0,
    }
}

fn count_pixels_adjacent_to_line(line_width: i32, b: &LBox, nonline: &Bitmap) -> u64 {
    let (mut x, mut y, mut bw, mut bh) = (b.x, b.y, b.w, b.h);
    if bw > bh {
        let bottom = (nonline.height as i32).min(y + bh + line_width);
        y = (y - line_width).max(0);
        bh = bottom - y;
    } else {
        let right = (nonline.width as i32).min(x + bw + line_width);
        x = (x - line_width).max(0);
        bw = right - x;
    }
    nonline.count_in(&LBox { x, y, w: bw, h: bh })
}

/// `FilterFalsePositives`: clears rejected components; returns how many
/// remain.
fn filter_false_positives(
    resolution: i32,
    nonline: &Bitmap,
    inter: Option<&Bitmap>,
    line: &mut Bitmap,
) -> usize {
    let min_thick_length = (f64::from(resolution) * THICK_LENGTH_MULTIPLE) as i32;
    let (boxes, comps) = line.conn_comp(8, true);
    let mut remaining = boxes.len();
    for (b, comp) in boxes.iter().zip(&comps) {
        let max_width = max_stroke_width(comp);
        let mut bad = b.w >= MIN_THICK_LINE_WIDTH
            && b.h >= MIN_THICK_LINE_WIDTH
            && b.w < min_thick_length
            && b.h < min_thick_length
            && max_width > MIN_THICK_LINE_WIDTH;
        if !bad && num_touching_intersections(b, inter) < 2 {
            let nonline_count = count_pixels_adjacent_to_line(max_width, b, nonline);
            if nonline_count as f64 > f64::from(b.h) * f64::from(b.w) * MAX_NON_LINE_DENSITY {
                bad = true;
            }
        }
        if bad {
            line.clear_rect(b);
            remaining -= 1;
        }
    }
    remaining
}

/// `GetLineMasks` (no music mask: `pageseg_apply_music_mask` is off).
pub fn line_masks(resolution: i32, src: &Bitmap) -> LineMasks {
    let max_line_width = resolution / THIN_LINE_FRACTION;
    let min_line_length = resolution / MIN_LINE_LENGTH_FRACTION;
    let closing_brick = max_line_width / 3;
    let closed = src.close_brick(closing_brick, closing_brick);
    let solid = closed.open_brick(max_line_width, max_line_width);
    let hollow = closed.subtract(&solid);
    let mut vline = Some(hollow.open_brick(1, min_line_length));
    let mut hline = Some(hollow.open_brick(min_line_length, 1));
    let v_empty = vline.as_ref().is_none_or(Bitmap::is_zero);
    let h_empty = hline.as_ref().is_none_or(Bitmap::is_zero);
    let mut nonlines: Option<Bitmap> = None;
    let mut intersections: Option<Bitmap> = None;
    let mut extra_non_hlines: Option<Bitmap> = None;
    let mut non_vline: Option<Bitmap> = None;
    let mut non_hline: Option<Bitmap> = None;
    if !v_empty {
        let vl = vline.as_ref().expect("checked");
        let mut nl = src.subtract(vl);
        if !h_empty {
            let hl = hline.as_ref().expect("checked");
            nl.combine(hl, Op::AndNot);
            let inter = vl.and(hl);
            extra_non_hlines = Some(vl.subtract(&inter));
            intersections = Some(inter);
        }
        let eroded = nl.erode_brick(MAX_LINE_RESIDUE, 1);
        let mut nv = Bitmap::seedfill(&eroded, &nl, 8);
        if !h_empty {
            nv.combine(hline.as_ref().expect("checked"), Op::Or);
            nv.combine(intersections.as_ref().expect("set"), Op::AndNot);
        }
        let keep = filter_false_positives(
            resolution,
            &nv,
            intersections.as_ref(),
            vline.as_mut().expect("checked"),
        );
        if keep == 0 {
            vline = None;
        }
        non_vline = Some(nv);
        nonlines = Some(nl);
    } else {
        vline = None;
        if !h_empty {
            nonlines = Some(src.subtract(hline.as_ref().expect("checked")));
        }
    }
    if h_empty {
        hline = None;
    } else {
        let nl = nonlines.as_ref().expect("set when h not empty");
        let eroded = nl.erode_brick(1, MAX_LINE_RESIDUE);
        let mut nh = Bitmap::seedfill(&eroded, nl, 8);
        if let Some(extra) = &extra_non_hlines {
            nh.combine(extra, Op::Or);
        }
        let keep = filter_false_positives(
            resolution,
            &nh,
            intersections.as_ref(),
            hline.as_mut().expect("checked"),
        );
        if keep == 0 {
            hline = None;
        }
        non_hline = Some(nh);
    }
    LineMasks {
        vline,
        non_vline,
        hline,
        non_hline,
        intersections,
    }
}

const CRACK_SPACING: usize = 100;
const LINE_FIND_GRID_SIZE: i32 = 50;

/// Ruling lines found and removed by [`find_and_remove_lines`].
#[derive(Default, Debug)]
pub struct Lines {
    pub vertical_x: i32,
    pub vertical_y: i32,
    pub vectors: TabVectors,
    pub v_lines: EList<VecId>,
    pub h_lines: EList<VecId>,
}

/// `GetLineBoxes`: cracks `lines` in place and returns line blobs whose
/// boxes are in Tesseract coordinates (x/y flipped for horizontal lines).
fn get_line_boxes(
    horizontal: bool,
    lines: &mut Bitmap,
    inter: Option<&Bitmap>,
    blobs: &mut Blobs,
) -> EList<BlobId> {
    let (width, height) = (lines.width, lines.height);
    if horizontal {
        for y in 0..height {
            let mut x = CRACK_SPACING;
            while x < width {
                lines.clear(x, y);
                x += CRACK_SPACING;
            }
        }
    } else {
        let mut y = CRACK_SPACING;
        while y < height {
            lines.row_mut(y).fill(0);
            y += CRACK_SPACING;
        }
    }
    let (boxes, _) = lines.conn_comp(8, false);
    // ConvertBoxaToBlobs: step-less outlines from the boxes.
    let outlines: Vec<Outline> = boxes
        .iter()
        .map(|b| Outline {
            start: ICoord::new(b.x, b.y),
            steps: Vec::new(),
            bbox: TBox::new(b.x, b.y, b.x + b.w, b.y + b.h),
            children: Vec::new(),
            inverse: false,
        })
        .collect();
    let (good, _) = outlines_to_blobs(width as i32, height as i32, outlines);
    let mut list = EList::new();
    let h = height as i32;
    for c in good {
        let mut nb = BlobNBox::new(c);
        let bb = nb.bbox;
        nb.line_crossings = num_touching_intersections(
            &LBox {
                x: bb.left,
                y: bb.bottom,
                w: bb.width(),
                h: bb.height(),
            },
            inter,
        ) as i32;
        nb.bbox = if horizontal {
            TBox::new(h - bb.top, bb.left, h - bb.bottom, bb.right)
        } else {
            TBox::new(bb.left, h - bb.top, bb.right, h - bb.bottom)
        };
        list.push_back(blobs.add(nb));
    }
    list
}

/// `FindLineVectors`.
fn find_line_vectors(
    tright: ICoord,
    blobs: &mut Blobs,
    line_bblobs: &EList<BlobId>,
    vertical_x: &mut i32,
    vertical_y: &mut i32,
    vectors: &mut TabVectors,
    out: &mut EList<VecId>,
) {
    let ids = line_bblobs.to_vec();
    let mut grid: BBGrid<BlobId> = BBGrid::new(LINE_FIND_GRID_SIZE, ICoord::new(0, 0), tright);
    for &id in &ids {
        let b = blobs.get_mut(id);
        b.left_tab_type = TabType::MaybeAligned;
        b.left_rule = 0;
        b.right_rule = tright.x;
        b.left_crossing_rule = 0;
        b.right_crossing_rule = tright.x;
        grid.insert_bbox(&*blobs, false, true, id);
    }
    if ids.is_empty() {
        return;
    }
    *vertical_x = 0;
    *vertical_y = 1;
    let mut search = GridSearch::new();
    search.start_full_search(&grid);
    while let Some(b) = search.next_full_search(&grid, &*blobs) {
        if blobs.get(b).left_tab_type == TabType::MaybeAligned {
            let width = blobs.get(b).bbox.width();
            let p = AlignedBlobParams::for_vline(*vertical_x, *vertical_y, width);
            if let Some(v) =
                find_vertical_alignment(&grid, blobs, vectors, &p, b, vertical_x, vertical_y)
            {
                vectors.get_mut(v).freeze();
                let mut it = Iter::new(out);
                it.add_to_end(out, v);
            }
        }
    }
}

/// `RemoveUnusedLineSegments`.
fn remove_unused_line_segments(
    horizontal: bool,
    blobs: &Blobs,
    line_bblobs: &EList<BlobId>,
    line_pix: &mut Bitmap,
) {
    let height = line_pix.height as i32;
    for id in line_bblobs.to_vec() {
        let b = blobs.get(id);
        if b.left_tab_type != TabType::VLine {
            let bx = b.bbox;
            let r = if horizontal {
                LBox {
                    x: bx.bottom,
                    y: height - bx.right,
                    w: bx.height(),
                    h: bx.width(),
                }
            } else {
                LBox {
                    x: bx.left,
                    y: height - bx.top,
                    w: bx.width(),
                    h: bx.height(),
                }
            };
            line_pix.clear_rect(&r);
        }
    }
}

/// `SubtractLinesAndResidue`.
fn subtract_lines_and_residue(line_pix: &Bitmap, non_line_pix: &Bitmap, src: &mut Bitmap) {
    *src = src.subtract(line_pix);
    let residue = src.subtract(non_line_pix);
    let fat = line_pix.dilate_brick(3, 3);
    let fat = Bitmap::seedfill(&fat, &residue, 8);
    *src = src.subtract(&fat);
}

/// `FindAndRemoveVLines` / `FindAndRemoveHLines`.
#[allow(clippy::too_many_arguments)]
fn find_and_remove(
    horizontal: bool,
    inter: Option<&Bitmap>,
    vertical_x: &mut i32,
    vertical_y: &mut i32,
    line_pix: &mut Option<Bitmap>,
    non_line_pix: Option<&Bitmap>,
    src: &mut Bitmap,
    vectors: &mut TabVectors,
    out: &mut EList<VecId>,
) {
    let Some(lp) = line_pix.as_mut() else {
        return;
    };
    let mut blobs = Blobs::default();
    let line_bblobs = get_line_boxes(horizontal, lp, inter, &mut blobs);
    let (w, h) = (src.width as i32, src.height as i32);
    let tright = if horizontal {
        ICoord::new(h, w)
    } else {
        ICoord::new(w, h)
    };
    find_line_vectors(
        tright,
        &mut blobs,
        &line_bblobs,
        vertical_x,
        vertical_y,
        vectors,
        out,
    );
    if !out.is_empty() {
        remove_unused_line_segments(horizontal, &blobs, &line_bblobs, lp);
        if let Some(nl) = non_line_pix {
            subtract_lines_and_residue(lp, nl, src);
        }
        let vertical = shrink(*vertical_x, *vertical_y);
        vectors.merge_similar_tab_vectors(&blobs, vertical, out, None);
        if horizontal {
            for v in out.to_vec() {
                vectors.get_mut(v).xy_flip();
            }
        }
    } else {
        *line_pix = None;
    }
}

/// `ICOORD::set_with_shrink`.
pub fn shrink(x: i32, y: i32) -> ICoord {
    let max_extent = x.abs().max(y.abs());
    let mut factor = 1;
    if max_extent > i32::from(i16::MAX) {
        factor = max_extent / i32::from(i16::MAX) + 1;
    }
    ICoord::new(x / factor, y / factor)
}

/// `LineFinder::FindAndRemoveLines` (without the music mask).
pub fn find_and_remove_lines(resolution: i32, pix: &mut Bitmap) -> Lines {
    let mut out = Lines {
        vertical_x: 0,
        vertical_y: 1,
        ..Lines::default()
    };
    let masks = line_masks(resolution, pix);
    let LineMasks {
        mut vline,
        non_vline,
        mut hline,
        non_hline,
        intersections,
    } = masks;
    let (mut vx, mut vy) = (out.vertical_x, out.vertical_y);
    find_and_remove(
        false,
        intersections.as_ref(),
        &mut vx,
        &mut vy,
        &mut vline,
        non_vline.as_ref(),
        pix,
        &mut out.vectors,
        &mut out.v_lines,
    );
    out.vertical_x = vx;
    out.vertical_y = vy;
    let mut intersections = None;
    if let Some(hl) = hline.as_mut() {
        if let Some(vl) = &vline {
            intersections = Some(vl.and(hl));
        }
        let nh = non_hline.as_ref().expect("non_hline exists with hline");
        if filter_false_positives(resolution, nh, intersections.as_ref(), hl) == 0 {
            hline = None;
        }
    }
    // Horizontal lines use copies of the vertical direction.
    let (mut hx, mut hy) = (out.vertical_x, out.vertical_y);
    find_and_remove(
        true,
        intersections.as_ref(),
        &mut hx,
        &mut hy,
        &mut hline,
        non_hline.as_ref(),
        pix,
        &mut out.vectors,
        &mut out.h_lines,
    );
    if let (Some(vl), Some(hl)) = (&vline, &hline) {
        let inter = vl.and(hl);
        let residue = inter.dilate_brick(5, 5);
        let residue = Bitmap::seedfill(&residue, pix, 8);
        *pix = pix.subtract(&residue);
    }
    out
}
