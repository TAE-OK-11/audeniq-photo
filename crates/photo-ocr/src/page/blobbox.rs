//! `BLOBNBOX`, `TO_BLOCK` and `Textord::find_components` (`tordmain.cpp`).

use super::bitmap::Bitmap;
use super::elist::{EList, Iter};
use super::geom::TBox;
use super::outline::{CBlob, outlines_to_blobs, trace_outlines};
use super::stats::Stats;

pub type BlobId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TabType {
    None,
    Deleted,
    MaybeRagged,
    MaybeAligned,
    Confirmed,
    VLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegionType {
    Noise,
    HLine,
    VLine,
    RectImage,
    PolyImage,
    Unknown,
    VertText,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowType {
    None,
    NonText,
    Neighbours,
    Chain,
    StrongChain,
    TextOnImage,
    Leader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecialText {
    None,
    Italic,
    Digit,
    Math,
    Unclear,
    Skip,
}

/// `BLOBNBOX`.
#[derive(Clone, Debug)]
pub struct BlobNBox {
    pub cblob: Option<CBlob>,
    pub bbox: TBox,
    pub red_box: TBox,
    pub area: i32,
    pub repeated_set: i32,
    pub left_tab_type: TabType,
    pub right_tab_type: TabType,
    pub region_type: RegionType,
    pub flow: FlowType,
    pub spt_type: SpecialText,
    pub joined: bool,
    pub reduced: bool,
    pub left_rule: i32,
    pub right_rule: i32,
    pub left_crossing_rule: i32,
    pub right_crossing_rule: i32,
    pub base_char_top: i32,
    pub base_char_bottom: i32,
    pub baseline_y: i32,
    pub line_crossings: i32,
    pub base_char_blob: Option<BlobId>,
    pub owner: Option<u32>,
    pub neighbours: [Option<BlobId>; 4],
    pub good_stroke_neighbours: [bool; 4],
    pub horz_possible: bool,
    pub vert_possible: bool,
    pub leader_on_left: bool,
    pub leader_on_right: bool,
    pub horz_stroke_width: f32,
    pub vert_stroke_width: f32,
    pub area_stroke_width: f32,
}

impl BlobNBox {
    pub fn new(cblob: CBlob) -> BlobNBox {
        let bbox = cblob.bounding_box();
        let area = cblob.area();
        let mut b = BlobNBox {
            bbox,
            red_box: TBox::default(),
            area,
            repeated_set: 0,
            left_tab_type: TabType::None,
            right_tab_type: TabType::None,
            region_type: RegionType::Unknown,
            flow: FlowType::None,
            spt_type: SpecialText::Skip,
            joined: false,
            reduced: false,
            left_rule: 0,
            right_rule: 0,
            left_crossing_rule: 0,
            right_crossing_rule: 0,
            base_char_top: bbox.top,
            base_char_bottom: bbox.bottom,
            baseline_y: bbox.bottom,
            line_crossings: 0,
            base_char_blob: None,
            owner: None,
            neighbours: [None; 4],
            good_stroke_neighbours: [false; 4],
            horz_possible: false,
            vert_possible: false,
            leader_on_left: false,
            leader_on_right: false,
            horz_stroke_width: 0.0,
            vert_stroke_width: 0.0,
            area_stroke_width: 0.0,
            cblob: None,
        };
        let perimeter = cblob.perimeter();
        if area > 0 && perimeter != 0 {
            b.area_stroke_width = 2.0 * area as f32 / perimeter as f32;
        }
        b.cblob = Some(cblob);
        b
    }

    pub fn enclosed_area(&self) -> i32 {
        self.area
    }
}

/// The arena owning every `BLOBNBOX` of a page; lists hold ids.
#[derive(Default, Debug)]
pub struct Blobs {
    pub boxes: Vec<BlobNBox>,
}

impl Blobs {
    pub fn add(&mut self, b: BlobNBox) -> BlobId {
        self.boxes.push(b);
        (self.boxes.len() - 1) as BlobId
    }

    pub fn get(&self, id: BlobId) -> &BlobNBox {
        &self.boxes[id as usize]
    }

    pub fn get_mut(&mut self, id: BlobId) -> &mut BlobNBox {
        &mut self.boxes[id as usize]
    }
}

/// `TO_BLOCK` (for the single page block of the automatic layout modes).
#[derive(Debug, Default)]
pub struct ToBlock {
    pub block_box: TBox,
    pub blobs: EList<BlobId>,
    pub underlines: EList<BlobId>,
    pub noise_blobs: EList<BlobId>,
    pub small_blobs: EList<BlobId>,
    pub large_blobs: EList<BlobId>,
    pub line_spacing: f32,
    pub line_size: f32,
    pub max_blob_size: f32,
}

const TEXTORD_MAX_NOISE_SIZE: i32 = 7;
const TEXTORD_NOISE_AREA_RATIO: f64 = 0.7;
const TEXTORD_INITIALX_ILE: f64 = 0.75;
const TEXTORD_INITIALASC_ILE: f64 = 0.90;
const TEXTORD_WIDTH_LIMIT: f64 = 8.0;
const TEXTORD_MIN_LINESIZE: f64 = 1.25;
const TEXTORD_EXCESS_BLOBSIZE: f64 = 1.3;
const MAX_NEAREST_DIST: i32 = 600;

pub const DESCENDER_FRACTION: f64 = 0.25;
pub const XHEIGHT_FRACTION: f64 = 0.5;
pub const ASCENDER_FRACTION: f64 = 0.25;
pub const XHEIGHT_CAP_RATIO: f64 = XHEIGHT_FRACTION / (XHEIGHT_FRACTION + ASCENDER_FRACTION);

/// `SetBlobStrokeWidth`.
fn set_blob_stroke_width(pix: &Bitmap, blob: &mut BlobNBox) {
    let b = blob.bbox;
    let (width, height) = (b.width(), b.height());
    let clip = pix.clip(&super::morph::LBox {
        x: b.left,
        y: pix.height as i32 - b.top,
        w: width,
        h: height,
    });
    let Some(clip) = clip else {
        return;
    };
    // pixClipRectangle may return a smaller pix at the image border.
    let (cw, ch) = (clip.width as i32, clip.height as i32);
    let dist = clip.distance_4();
    let at = |x: i32, y: i32| i32::from(dist[(y * cw + x) as usize]);
    let (width, height) = (width.min(cw), height.min(ch));
    let mut h_stats = Stats::new(0, b.width());
    for y in 0..height {
        let mut prev = 0;
        let mut pixel = at(0, y);
        for x in 1..width {
            let next = at(x, y);
            if prev < pixel
                && (y == 0 || pixel == at(x - 1, y - 1))
                && (y == height - 1 || pixel == at(x - 1, y + 1))
            {
                if pixel > next {
                    h_stats.add(pixel * 2 - 1, 1);
                } else if pixel == next && x + 1 < width && pixel > at(x + 1, y) {
                    h_stats.add(pixel * 2, 1);
                }
            }
            prev = pixel;
            pixel = next;
        }
    }
    let mut v_stats = Stats::new(0, b.height());
    for x in 0..width {
        let mut prev = 0;
        let mut pixel = at(x, 0);
        for y in 1..height {
            let next = at(x, y);
            if prev < pixel
                && (x == 0 || pixel == at(x - 1, y - 1))
                && (x == width - 1 || pixel == at(x + 1, y - 1))
            {
                if pixel > next {
                    v_stats.add(pixel * 2 - 1, 1);
                } else if pixel == next && y + 1 < height && pixel > at(x, y + 1) {
                    v_stats.add(pixel * 2, 1);
                }
            }
            prev = pixel;
            pixel = next;
        }
    }
    let quarter = (b.width() + b.height()) / 4;
    if h_stats.get_total() >= quarter {
        blob.horz_stroke_width = h_stats.ile(0.5) as f32;
        blob.vert_stroke_width = if v_stats.get_total() >= quarter {
            v_stats.ile(0.5) as f32
        } else {
            0.0
        };
    } else if v_stats.get_total() >= quarter || v_stats.get_total() > h_stats.get_total() {
        blob.horz_stroke_width = 0.0;
        blob.vert_stroke_width = v_stats.ile(0.5) as f32;
    } else {
        blob.horz_stroke_width = if h_stats.get_total() > 2 {
            h_stats.ile(0.5) as f32
        } else {
            0.0
        };
        blob.vert_stroke_width = 0.0;
    }
}

/// `filter_noise_blobs`: returns the initial x-height estimate.
fn filter_noise_blobs(blobs: &Blobs, b: &mut ToBlock) -> f32 {
    let bx = |id: BlobId| blobs.get(id).bbox;
    let mut src_it = Iter::new(&b.blobs);
    let mut noise_it = Iter::new(&b.noise_blobs);
    let mut small_it = Iter::new(&b.small_blobs);
    let mut large_it = Iter::new(&b.large_blobs);
    let mut size_stats = Stats::new(0, MAX_NEAREST_DIST - 1);
    src_it.mark_cycle_pt();
    while !src_it.cycled_list(&b.blobs) {
        let id = src_it.data(&b.blobs);
        let bb = bx(id);
        if bb.height() < TEXTORD_MAX_NOISE_SIZE {
            let v = src_it.extract(&mut b.blobs);
            noise_it.add_after_then_move(&mut b.noise_blobs, v);
        } else if f64::from(blobs.get(id).enclosed_area())
            >= f64::from(bb.height()) * f64::from(bb.width()) * TEXTORD_NOISE_AREA_RATIO
        {
            let v = src_it.extract(&mut b.blobs);
            small_it.add_after_then_move(&mut b.small_blobs, v);
        }
        src_it.forward(&b.blobs);
    }
    src_it.mark_cycle_pt();
    while !src_it.cycled_list(&b.blobs) {
        size_stats.add(bx(src_it.data(&b.blobs)).height(), 1);
        src_it.forward(&b.blobs);
    }
    let mut initial_x = size_stats.ile(TEXTORD_INITIALX_ILE) as f32;
    let max_y = (f64::from(initial_x)
        * (DESCENDER_FRACTION + XHEIGHT_FRACTION + 2.0 * ASCENDER_FRACTION)
        / XHEIGHT_FRACTION)
        .ceil() as f32;
    let min_y = (initial_x / 2.0).floor();
    let max_x = (f64::from(initial_x) * TEXTORD_WIDTH_LIMIT).ceil() as f32;
    small_it.move_to_first(&b.small_blobs);
    small_it.mark_cycle_pt();
    while !small_it.cycled_list(&b.small_blobs) {
        let height = bx(small_it.data(&b.small_blobs)).height() as f32;
        if height > max_y {
            let v = small_it.extract(&mut b.small_blobs);
            large_it.add_after_then_move(&mut b.large_blobs, v);
        } else if height >= min_y {
            let v = small_it.extract(&mut b.small_blobs);
            src_it.add_after_then_move(&mut b.blobs, v);
        }
        small_it.forward(&b.small_blobs);
    }
    size_stats.clear();
    src_it.mark_cycle_pt();
    while !src_it.cycled_list(&b.blobs) {
        let bb = bx(src_it.data(&b.blobs));
        let (height, width) = (bb.height() as f32, bb.width() as f32);
        if height < min_y {
            let v = src_it.extract(&mut b.blobs);
            small_it.add_after_then_move(&mut b.small_blobs, v);
        } else if height > max_y || width > max_x {
            let v = src_it.extract(&mut b.blobs);
            large_it.add_after_then_move(&mut b.large_blobs, v);
        } else {
            size_stats.add(bb.height(), 1);
        }
        src_it.forward(&b.blobs);
    }
    let mut max_height = size_stats.ile(TEXTORD_INITIALASC_ILE) as f32;
    max_height = (f64::from(max_height) * XHEIGHT_CAP_RATIO) as f32;
    if max_height > initial_x {
        initial_x = max_height;
    }
    initial_x
}

/// `Textord::find_components` for one block covering the page.
pub fn find_components(pix: &Bitmap, blobs: &mut Blobs) -> Option<ToBlock> {
    let (w, h) = (pix.width as i32, pix.height as i32);
    if w > i32::from(i16::MAX) || h > i32::from(i16::MAX) {
        return None;
    }
    let outlines = trace_outlines(pix);
    let (good, bad) = outlines_to_blobs(w, h, outlines);
    let mut tb = ToBlock {
        block_box: TBox::new(0, 0, w, h),
        ..ToBlock::default()
    };
    for (list, cblobs) in [(&mut tb.blobs, good), (&mut tb.noise_blobs, bad)] {
        for c in cblobs {
            let mut nb = BlobNBox::new(c);
            set_blob_stroke_width(pix, &mut nb);
            list.push_back(blobs.add(nb));
        }
    }
    // filter_blobs
    tb.line_size = filter_noise_blobs(blobs, &mut tb);
    if tb.line_size == 0.0 {
        tb.line_size = 1.0;
    }
    tb.line_spacing = (f64::from(tb.line_size)
        * (DESCENDER_FRACTION + XHEIGHT_FRACTION + 2.0 * ASCENDER_FRACTION)
        / XHEIGHT_FRACTION) as f32;
    tb.line_size = (f64::from(tb.line_size) * TEXTORD_MIN_LINESIZE) as f32;
    tb.max_blob_size = (f64::from(tb.line_size) * TEXTORD_EXCESS_BLOBSIZE) as f32;
    Some(tb)
}
