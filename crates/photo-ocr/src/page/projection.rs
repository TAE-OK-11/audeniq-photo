//! `TextlineProjection` (`textlineprojection.cpp`): a blurred projection of
//! padded blob boxes used to judge whether boxes sit inside textlines. The
//! layout path here has no rotation and an identity `DENORM`.

use super::bitmap::Bitmap;
use super::blobbox::{BND_ABOVE, BND_BELOW, BND_LEFT, BND_RIGHT, BlobId, Blobs};
use super::colpartition::{PartId, Parts};
use super::elist::{EList, Iter};
use super::geom::TBox;
use super::lept::{Gray8, blockconv_in_place, box_create, clip_box_to_foreground};

const ORIENTED_PAD_FACTOR: i32 = 8;
const DEFAULT_PAD_FACTOR: i32 = 2;
const WRONG_WAY_PENALTY: i32 = 4;
const PARA_PERP_DIST_RATIO: i32 = 4;
const MIN_LINE_SPACING_FACTOR: i32 = 4;
const MAX_TAB_STOP_OVERRUN: i32 = 6;

pub fn div_rounded(a: i32, b: i32) -> i32 {
    if b < 0 {
        return -div_rounded(a, -b);
    }
    if a >= 0 {
        (a + b / 2) / b
    } else {
        (a - b / 2) / b
    }
}

pub struct TextlineProjection {
    x_origin: i32,
    y_origin: i32,
    scale_factor: i32,
    pix: Gray8,
}

impl TextlineProjection {
    pub fn new(resolution: i32) -> TextlineProjection {
        let mut scale_factor = super::detlinefit::int_cast_rounded(f64::from(resolution) / 100.0);
        if scale_factor < 1 {
            scale_factor = 1;
        }
        TextlineProjection {
            x_origin: 0,
            y_origin: 0,
            scale_factor,
            pix: Gray8::new(0, 0),
        }
    }

    /// `ConstructProjection` (no rotation).
    pub fn construct_projection(
        &mut self,
        blobs: &Blobs,
        lists: [&EList<BlobId>; 2],
        nontext_map: &Bitmap,
    ) {
        let image_box = TBox::new(0, 0, nontext_map.width as i32, nontext_map.height as i32);
        self.x_origin = 0;
        self.y_origin = image_box.height();
        let w = (image_box.width() + self.scale_factor - 1) / self.scale_factor;
        let h = (image_box.height() + self.scale_factor - 1) / self.scale_factor;
        self.pix = Gray8::new(w as usize, h as usize);
        for list in lists {
            self.project_blobs(blobs, list, &image_box, nontext_map);
        }
        blockconv_in_place(&mut self.pix, 1, 1);
    }

    /// `MoveNonTextlineBlobs`.
    pub fn move_non_textline_blobs(
        &self,
        blobs: &mut Blobs,
        list: &mut EList<BlobId>,
        small: &mut EList<BlobId>,
    ) {
        let mut it = Iter::new(list);
        let mut small_it = Iter::new(small);
        it.mark_cycle_pt();
        while !it.cycled_list(list) {
            let b = it.data(list);
            let bx = blobs.get(b).bbox;
            if self.box_out_of_htextline(&bx) && !blobs.get(b).uniquely_vertical() {
                blobs.get_mut(b).clear_neighbours();
                let v = it.extract(list);
                small_it.add_to_end(small, v);
            }
            it.forward(list);
        }
    }

    /// `DistanceOfBoxFromBox`.
    pub fn distance_of_box_from_box(
        &self,
        from: &TBox,
        to: &TBox,
        horizontal_textline: bool,
    ) -> i32 {
        let parallel_gap;
        let (mut sx, mut sy, mut ex, mut ey);
        if horizontal_textline {
            parallel_gap = from.x_gap(to) + from.width();
            sx = (from.left + from.right) / 2;
            ex = sx;
            if from.top - to.top >= to.bottom - from.bottom {
                sy = from.top;
                ey = to.top.min(sy);
            } else {
                sy = from.bottom;
                ey = to.bottom.max(sy);
            }
        } else {
            parallel_gap = from.y_gap(to) + from.height();
            if from.right - to.right >= to.left - from.left {
                sx = from.right;
                ex = to.right.min(sx);
            } else {
                sx = from.left;
                ex = to.left.max(sx);
            }
            sy = (from.bottom + from.top) / 2;
            ey = sy;
        }
        // TPOINT holds 16-bit coordinates.
        sx = i32::from(sx as i16);
        sy = i32::from(sy as i16);
        ex = i32::from(ex as i16);
        ey = i32::from(ey as i16);
        let mut perpendicular_gap = 0;
        if sx != ex || sy != ey {
            if (sy - ey).abs() >= (sx - ex).abs() {
                perpendicular_gap = self.vertical_distance(sx, sy, ey);
            } else {
                perpendicular_gap = self.horizontal_distance(sx, ex, sy);
            }
        }
        perpendicular_gap + parallel_gap / PARA_PERP_DIST_RATIO
    }

    fn vertical_distance(&self, x: i32, y1: i32, y2: i32) -> i32 {
        let x = self.image_x_to_projection_x(x);
        let y1 = self.image_y_to_projection_y(y1);
        let y2 = self.image_y_to_projection_y(y2);
        if y1 == y2 {
            return 0;
        }
        let step = if y1 < y2 { 1 } else { -1 };
        let mut prev = i32::from(self.pix.get(x as usize, y1 as usize));
        let mut distance = 0;
        let mut right_way_steps = 0;
        let mut y = y1;
        while y != y2 {
            let pixel = i32::from(self.pix.get(x as usize, (y + step) as usize));
            if pixel < prev {
                distance += WRONG_WAY_PENALTY;
            } else if pixel > prev {
                right_way_steps += 1;
            } else {
                distance += 1;
            }
            prev = pixel;
            y += step;
        }
        distance * self.scale_factor + right_way_steps * self.scale_factor / WRONG_WAY_PENALTY
    }

    fn horizontal_distance(&self, x1: i32, x2: i32, y: i32) -> i32 {
        let x1 = self.image_x_to_projection_x(x1);
        let x2 = self.image_x_to_projection_x(x2);
        let y = self.image_y_to_projection_y(y);
        if x1 == x2 {
            return 0;
        }
        let step = if x1 < x2 { 1 } else { -1 };
        let mut prev = i32::from(self.pix.get(x1 as usize, y as usize));
        let mut distance = 0;
        let mut right_way_steps = 0;
        let mut x = x1;
        while x != x2 {
            let pixel = i32::from(self.pix.get((x + step) as usize, y as usize));
            if pixel < prev {
                distance += WRONG_WAY_PENALTY;
            } else if pixel > prev {
                right_way_steps += 1;
            } else {
                distance += 1;
            }
            prev = pixel;
            x += step;
        }
        distance * self.scale_factor + right_way_steps * self.scale_factor / WRONG_WAY_PENALTY
    }

    /// `BoxOutOfHTextline`.
    pub fn box_out_of_htextline(&self, b: &TBox) -> bool {
        let (grad1, grad2) = self.evaluate_box_gradients(b);
        let worst = grad1.min(grad2);
        let total = grad1 + grad2;
        if total >= 6 {
            return false;
        }
        worst < 0
    }

    /// `EvaluateColPartition`.
    pub fn evaluate_col_partition(&self, parts: &Parts, id: PartId) -> i32 {
        let p = parts.get(id);
        if p.is_singleton() {
            return self.evaluate_box(&p.bounding_box);
        }
        let mut b = p.bounding_box;
        b.left = p.median_left;
        b.right = p.median_right;
        let vresult = self.evaluate_box(&b);
        let mut b = p.bounding_box;
        b.top = p.median_top;
        b.bottom = p.median_bottom;
        let hresult = self.evaluate_box(&b);
        if hresult >= -vresult {
            hresult
        } else {
            vresult
        }
    }

    pub fn evaluate_box(&self, b: &TBox) -> i32 {
        self.evaluate_box_internal(b).0
    }

    fn evaluate_box_gradients(&self, b: &TBox) -> (i32, i32) {
        let r = self.evaluate_box_internal(b);
        (r.1, r.2)
    }

    /// `EvaluateBoxInternal`: (result, top_gradient, bottom_gradient).
    fn evaluate_box_internal(&self, b: &TBox) -> (i32, i32, i32) {
        let t = |v: i32| i32::from(v as i16);
        let top = self.best_mean_gradient_in_row(t(b.left), t(b.right), t(b.top), true);
        let bottom = -self.best_mean_gradient_in_row(t(b.left), t(b.right), t(b.bottom), false);
        let left = self.best_mean_gradient_in_column(t(b.left), t(b.bottom), t(b.top), true);
        let right = -self.best_mean_gradient_in_column(t(b.right), t(b.bottom), t(b.top), false);
        let result = top.max(0).max(bottom.max(0)) - left.max(0).max(right.max(0));
        (result, top, bottom)
    }

    fn best_mean_gradient_in_row(&self, min_x: i32, max_x: i32, y: i32, best_is_max: bool) -> i32 {
        let (s, e) = ((min_x, y), (max_x, y));
        let mut best =
            self.mean_pixels_in_line_segment(2, s, e) - self.mean_pixels_in_line_segment(-2, s, e);
        for (u, l) in [(-1, 3), (-3, 1)] {
            let g = self.mean_pixels_in_line_segment(l, s, e)
                - self.mean_pixels_in_line_segment(u, s, e);
            if (g > best) == best_is_max {
                best = g;
            }
        }
        best
    }

    fn best_mean_gradient_in_column(
        &self,
        x: i32,
        min_y: i32,
        max_y: i32,
        best_is_max: bool,
    ) -> i32 {
        let (s, e) = ((x, min_y), (x, max_y));
        let mut best =
            self.mean_pixels_in_line_segment(2, s, e) - self.mean_pixels_in_line_segment(-2, s, e);
        for (l, r) in [(-1, 3), (-3, 1)] {
            let g = self.mean_pixels_in_line_segment(r, s, e)
                - self.mean_pixels_in_line_segment(l, s, e);
            if (g > best) == best_is_max {
                best = g;
            }
        }
        best
    }

    fn truncate(&self, p: (i32, i32)) -> (i32, i32) {
        (
            p.0.clamp(0, self.pix.w as i32 - 1),
            p.1.clamp(0, self.pix.h as i32 - 1),
        )
    }

    /// `MeanPixelsInLineSegment`.
    fn mean_pixels_in_line_segment(
        &self,
        mut offset: i32,
        start: (i32, i32),
        end: (i32, i32),
    ) -> i32 {
        let mut s = (
            self.image_x_to_projection_x(start.0),
            self.image_y_to_projection_y(start.1),
        );
        let mut e = (
            self.image_x_to_projection_x(end.0),
            self.image_y_to_projection_y(end.1),
        );
        s = self.truncate(s);
        e = self.truncate(e);
        let mut total = 0;
        let count;
        let mut x_delta = e.0 - s.0;
        let mut y_delta = e.1 - s.1;
        if x_delta.abs() >= y_delta.abs() {
            if x_delta == 0 {
                return 0;
            }
            let x_step = if x_delta > 0 { 1 } else { -1 };
            offset *= x_step;
            s.1 += offset;
            e.1 += offset;
            s = self.truncate(s);
            e = self.truncate(e);
            x_delta = e.0 - s.0;
            y_delta = e.1 - s.1;
            count = x_delta * x_step + 1;
            let mut x = s.0;
            while x != e.0 {
                let y = s.1 + div_rounded(y_delta * (x - s.0), x_delta);
                total += i32::from(self.pix.get(x as usize, y as usize));
                x += x_step;
            }
        } else {
            let y_step = if y_delta > 0 { 1 } else { -1 };
            offset *= -y_step;
            s.0 += offset;
            e.0 += offset;
            s = self.truncate(s);
            e = self.truncate(e);
            x_delta = e.0 - s.0;
            y_delta = e.1 - s.1;
            count = y_delta * y_step + 1;
            let mut y = s.1;
            while y != e.1 {
                let x = s.0 + div_rounded(x_delta * (y - s.1), y_delta);
                total += i32::from(self.pix.get(x as usize, y as usize));
                y += y_step;
            }
        }
        div_rounded(total, count)
    }

    fn increment_rectangle(&mut self, b: &TBox) {
        let l = self.image_x_to_projection_x(b.left);
        let t = self.image_y_to_projection_y(b.top);
        let r = self.image_x_to_projection_x(b.right);
        let bt = self.image_y_to_projection_y(b.bottom);
        let w = self.pix.w;
        for y in t..=bt {
            for x in l..=r {
                let p = &mut self.pix.data[y as usize * w + x as usize];
                *p = p.saturating_add(1);
            }
        }
    }

    fn project_blobs(
        &mut self,
        blobs: &Blobs,
        list: &EList<BlobId>,
        map_box: &TBox,
        nontext_map: &Bitmap,
    ) {
        for b in list.to_vec() {
            let mut bbox = blobs.get(b).bbox;
            let (mx, my) = ((bbox.left + bbox.right) / 2, (bbox.bottom + bbox.top) / 2);
            let spreading_horizontally = self.pad_blob_box(blobs, b, &mut bbox);
            bbox = if bbox.overlap(map_box) {
                bbox.intersection(map_box)
            } else {
                TBox::new(
                    i32::from(i16::MAX),
                    i32::from(i16::MAX),
                    -i32::from(i16::MAX),
                    -i32::from(i16::MAX),
                )
            };
            truncate_box_to_miss_non_text(mx, my, spreading_horizontally, nontext_map, &mut bbox);
            if bbox.area() > 0 {
                self.increment_rectangle(&bbox);
            }
        }
    }

    fn pad_blob_box(&self, blobs: &Blobs, id: BlobId, bbox: &mut TBox) -> bool {
        let blob = blobs.get(id);
        let mut pad_limit = self.scale_factor * MIN_LINE_SPACING_FACTOR;
        let mut xpad = 0;
        let mut ypad = 0;
        let mut padding_horizontally = false;
        let nb = |d: usize| blob.neighbours[d];
        if blob.uniquely_horizontal() {
            xpad = bbox.height() * ORIENTED_PAD_FACTOR;
            padding_horizontally = true;
            if nb(BND_ABOVE).is_none_or(|n| bbox.y_gap(&blobs.get(n).bbox) > pad_limit)
                && nb(BND_BELOW).is_none_or(|n| bbox.y_gap(&blobs.get(n).bbox) > pad_limit)
            {
                ypad = self.scale_factor;
            }
        } else if blob.uniquely_vertical() {
            ypad = bbox.width() * ORIENTED_PAD_FACTOR;
            if nb(BND_LEFT).is_none_or(|n| bbox.x_gap(&blobs.get(n).bbox) > pad_limit)
                && nb(BND_RIGHT).is_none_or(|n| bbox.x_gap(&blobs.get(n).bbox) > pad_limit)
            {
                xpad = self.scale_factor;
            }
        } else {
            let mutual = |d: usize, back: usize| {
                nb(d).is_some_and(|n| blobs.get(n).neighbours[back] == Some(id))
            };
            if mutual(BND_ABOVE, BND_BELOW) || mutual(BND_BELOW, BND_ABOVE) {
                ypad = bbox.width() * DEFAULT_PAD_FACTOR;
            }
            if mutual(BND_RIGHT, BND_LEFT) || mutual(BND_LEFT, BND_RIGHT) {
                xpad = bbox.height() * DEFAULT_PAD_FACTOR;
                padding_horizontally = true;
            }
        }
        bbox.pad(xpad, ypad);
        pad_limit = self.scale_factor * MAX_TAB_STOP_OVERRUN;
        if bbox.left < blob.left_rule - pad_limit {
            bbox.left = blob.left_rule - pad_limit;
        }
        if bbox.right > blob.right_rule + pad_limit {
            bbox.right = blob.right_rule + pad_limit;
        }
        padding_horizontally
    }

    fn image_x_to_projection_x(&self, x: i32) -> i32 {
        ((x - self.x_origin) / self.scale_factor).clamp(0, self.pix.w as i32 - 1)
    }

    fn image_y_to_projection_y(&self, y: i32) -> i32 {
        ((self.y_origin - y) / self.scale_factor).clamp(0, self.pix.h as i32 - 1)
    }
}

/// `BoundsWithinBox`.
fn bounds_within_box(pix: &Bitmap, b: &TBox) -> TBox {
    let im_height = pix.height as i32;
    let input = box_create(b.left, im_height - b.top, b.width(), b.height());
    let mut r = TBox::default();
    if let Some(o) = clip_box_to_foreground(pix, input) {
        r.left = o.x;
        r.right = o.x + o.w;
        r.top = im_height - o.y;
        r.bottom = r.top - o.h;
    }
    r
}

/// `TruncateBoxToMissNonText`.
fn truncate_box_to_miss_non_text(
    x_middle: i32,
    y_middle: i32,
    split_on_x: bool,
    map: &Bitmap,
    bbox: &mut TBox,
) {
    let mut b1 = *bbox;
    let mut b2 = *bbox;
    if split_on_x {
        b1.right = x_middle;
        let im = bounds_within_box(map, &b1);
        if !im.null_box() {
            b1.left = im.right;
        }
        b2.left = x_middle;
        let im = bounds_within_box(map, &b2);
        if !im.null_box() {
            b2.right = im.left;
        }
    } else {
        b1.bottom = y_middle;
        let im = bounds_within_box(map, &b1);
        if !im.null_box() {
            b1.top = im.bottom;
        }
        b2.top = y_middle;
        let im = bounds_within_box(map, &b2);
        if !im.null_box() {
            b2.bottom = im.top;
        }
    }
    b1.union_with(&b2);
    *bbox = b1;
}
