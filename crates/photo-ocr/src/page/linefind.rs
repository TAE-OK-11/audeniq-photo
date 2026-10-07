//! `LineFinder`: horizontal/vertical ruling-line candidates
//! (`linefind.cpp`, mask stage).

use super::bitmap::Bitmap;
use super::morph::{LBox, Op};

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
