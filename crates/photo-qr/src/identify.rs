//! quirc identify.c, ported: threshold → regions → capstones → grids.

use crate::tables::VERSIONS;
use photo_core::{Deadline, Result};

const WHITE: u16 = 0;
const BLACK: u16 = 1;
const REGION0: u16 = 2;
const MAX_REGION: usize = 65_534;
const MAX_CAPSTONES: usize = 256;
const MAX_GRIDS: usize = 64;
const MAX_VERSION: i32 = 40;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug)]
struct Region {
    seed: Point,
    count: i64,
    capstone: i32,
}

#[derive(Clone, Copy, Debug)]
struct Capstone {
    corners: [Point; 4],
    center: Point,
    c: [f64; 8],
    qr_grid: i32,
}

#[derive(Clone, Debug)]
pub(crate) struct Grid {
    caps: [usize; 3],
    align_region: i32,
    align: Point,
    tpep: [Point; 3],
    grid_size: i32,
    c: [f64; 8],
}

/// Extracted module bitmap (true = dark).
pub(crate) struct Code {
    pub size: usize,
    pub cells: Vec<bool>,
}

impl Code {
    pub fn bit(&self, x: usize, y: usize) -> bool {
        self.cells[y * self.size + x]
    }
    /// Transpose (quirc_flip) for mirrored symbols.
    pub fn flipped(&self) -> Code {
        let n = self.size;
        let mut cells = vec![false; n * n];
        for y in 0..n {
            for x in 0..n {
                cells[x * n + y] = self.cells[y * n + x];
            }
        }
        Code { size: n, cells }
    }
}

pub(crate) struct Quirc {
    w: usize,
    h: usize,
    px: Vec<u16>,
    regions: Vec<Region>,
    caps: Vec<Capstone>,
    pub grids: Vec<Grid>,
}

fn perspective_setup(rect: &[Point; 4], w: f64, h: f64) -> [f64; 8] {
    let (x0, y0) = (f64::from(rect[0].x), f64::from(rect[0].y));
    let (x1, y1) = (f64::from(rect[1].x), f64::from(rect[1].y));
    let (x2, y2) = (f64::from(rect[2].x), f64::from(rect[2].y));
    let (x3, y3) = (f64::from(rect[3].x), f64::from(rect[3].y));
    let wden = w * (x2 * y3 - x3 * y2 + (x3 - x2) * y1 + x1 * (y2 - y3));
    let hden = h * (x2 * y3 + x1 * (y2 - y3) - x3 * y2 + (x3 - x2) * y1);
    [
        (x1 * (x2 * y3 - x3 * y2) + x0 * (-x2 * y3 + x3 * y2 + (x2 - x3) * y1) + x1 * (x3 - x2) * y0) / wden,
        -(x0 * (x2 * y3 + x1 * (y2 - y3) - x2 * y1) - x1 * x3 * y2 + x2 * x3 * y1 + (x1 * x3 - x2 * x3) * y0) / hden,
        x0,
        (y0 * (x1 * (y3 - y2) - x2 * y3 + x3 * y2) + y1 * (x2 * y3 - x3 * y2) + x0 * y1 * (y2 - y3)) / wden,
        (x0 * (y1 * y3 - y2 * y3) + x1 * y2 * y3 - x2 * y1 * y3 + y0 * (x3 * y2 - x1 * y2 + (x2 - x3) * y1)) / hden,
        y0,
        (x1 * (y3 - y2) + x0 * (y2 - y3) + (x2 - x3) * y1 + (x3 - x2) * y0) / wden,
        (-x2 * y3 + x1 * y3 + x3 * y2 + x0 * (y1 - y2) - x3 * y1 + (x2 - x1) * y0) / hden,
    ]
}

fn rint(v: f64) -> i32 {
    // C rint() with the default round-half-to-even mode.
    if !v.is_finite() {
        return i32::MIN;
    }
    v.round_ties_even().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

fn perspective_map(c: &[f64; 8], u: f64, v: f64) -> Point {
    let den = c[6] * u + c[7] * v + 1.0;
    let x = (c[0] * u + c[1] * v + c[2]) / den;
    let y = (c[3] * u + c[4] * v + c[5]) / den;
    Point { x: rint(x), y: rint(y) }
}

fn perspective_unmap(c: &[f64; 8], p: Point) -> (f64, f64) {
    let (x, y) = (f64::from(p.x), f64::from(p.y));
    let den = -c[0] * c[7] * y + c[1] * c[6] * y + (c[3] * c[7] - c[4] * c[6]) * x + c[0] * c[4] - c[1] * c[3];
    let u = -(c[1] * (y - c[5]) - c[2] * c[7] * y + (c[5] * c[7] - c[4]) * x + c[2] * c[4]) / den;
    let v = (c[0] * (y - c[5]) - c[2] * c[6] * y + (c[5] * c[6] - c[3]) * x + c[2] * c[3]) / den;
    (u, v)
}

fn line_intersect(p0: Point, p1: Point, q0: Point, q1: Point) -> Option<Point> {
    let a = -(i64::from(p1.y) - i64::from(p0.y));
    let b = i64::from(p1.x) - i64::from(p0.x);
    let c = -(i64::from(q1.y) - i64::from(q0.y));
    let d = i64::from(q1.x) - i64::from(q0.x);
    let e = a * i64::from(p1.x) + b * i64::from(p1.y);
    let f = c * i64::from(q1.x) + d * i64::from(q1.y);
    let det = a * d - b * c;
    if det == 0 {
        return None;
    }
    Some(Point { x: ((d * e - b * f) / det) as i32, y: ((-c * e + a * f) / det) as i32 })
}

/// Span visitor used by the flood fill.
trait Spans {
    fn span(&mut self, y: i32, left: i32, right: i32);
}

struct Count(i64);
impl Spans for Count {
    fn span(&mut self, _y: i32, left: i32, right: i32) {
        self.0 += i64::from(right - left + 1);
    }
}

struct NoSpans;
impl Spans for NoSpans {
    fn span(&mut self, _: i32, _: i32, _: i32) {}
}

struct OneCorner {
    reference: Point,
    best: i64,
    corner: Point,
}
impl Spans for OneCorner {
    fn span(&mut self, y: i32, left: i32, right: i32) {
        let dy = i64::from(y - self.reference.y);
        for x in [left, right] {
            let dx = i64::from(x - self.reference.x);
            let d = dx * dx + dy * dy;
            if d > self.best {
                self.best = d;
                self.corner = Point { x, y };
            }
        }
    }
}

struct OtherCorners {
    reference: Point,
    scores: [i64; 4],
    corners: [Point; 4],
}
impl Spans for OtherCorners {
    fn span(&mut self, y: i32, left: i32, right: i32) {
        for x in [left, right] {
            let up = i64::from(x) * i64::from(self.reference.x) + i64::from(y) * i64::from(self.reference.y);
            let r = i64::from(x) * -i64::from(self.reference.y) + i64::from(y) * i64::from(self.reference.x);
            let s = [up, r, -up, -r];
            for j in 0..4 {
                if s[j] > self.scores[j] {
                    self.scores[j] = s[j];
                    self.corners[j] = Point { x, y };
                }
            }
        }
    }
}

struct Leftmost {
    reference: Point,
    best: i64,
    corner: Point,
}
impl Spans for Leftmost {
    fn span(&mut self, y: i32, left: i32, right: i32) {
        for x in [left, right] {
            let d = -i64::from(self.reference.y) * i64::from(x) + i64::from(self.reference.x) * i64::from(y);
            if d < self.best {
                self.best = d;
                self.corner = Point { x, y };
            }
        }
    }
}

impl Quirc {
    pub fn new(gray: &[u8], w: usize, h: usize) -> Quirc {
        let mut q = Quirc { w, h, px: vec![WHITE; w * h], regions: Vec::new(), caps: Vec::new(), grids: Vec::new() };
        q.threshold(gray);
        q
    }

    /// quirc's adaptive threshold (boustrophedon moving average).
    fn threshold(&mut self, gray: &[u8]) {
        let (w, h) = (self.w, self.h);
        if w == 0 || h == 0 {
            return;
        }
        let s = (w / 8).max(1) as i64;
        let (mut avg_w, mut avg_u) = (0i64, 0i64);
        let mut row_avg = vec![0i64; w];
        for y in 0..h {
            row_avg.iter_mut().for_each(|v| *v = 0);
            let row = &gray[y * w..(y + 1) * w];
            for x in 0..w {
                let (wi, ui) = if y & 1 == 1 { (x, w - 1 - x) } else { (w - 1 - x, x) };
                avg_w = (avg_w * (s - 1)) / s + i64::from(row[wi]);
                avg_u = (avg_u * (s - 1)) / s + i64::from(row[ui]);
                row_avg[wi] += avg_w;
                row_avg[ui] += avg_u;
            }
            let out = &mut self.px[y * w..(y + 1) * w];
            for x in 0..w {
                out[x] = if i64::from(row[x]) < row_avg[x] * (100 - 5) / (200 * s) { BLACK } else { WHITE };
            }
        }
    }

    fn fill<S: Spans>(&mut self, x: i32, y: i32, from: u16, to: u16, spans: &mut S) {
        let (w, h) = (self.w as i32, self.h as i32);
        let mut stack = vec![(x, y)];
        while let Some((sx, sy)) = stack.pop() {
            let row = sy as usize * self.w;
            if self.px[row + sx as usize] != from {
                continue;
            }
            let mut left = sx;
            let mut right = sx;
            while left > 0 && self.px[row + left as usize - 1] == from {
                left -= 1;
            }
            while right < w - 1 && self.px[row + right as usize + 1] == from {
                right += 1;
            }
            for i in left..=right {
                self.px[row + i as usize] = to;
            }
            spans.span(sy, left, right);
            for ny in [sy - 1, sy + 1] {
                if ny < 0 || ny >= h {
                    continue;
                }
                let nrow = ny as usize * self.w;
                let mut i = left;
                while i <= right {
                    if self.px[nrow + i as usize] == from {
                        stack.push((i, ny));
                        while i <= right && self.px[nrow + i as usize] == from {
                            i += 1;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }

    fn region_code(&mut self, x: i32, y: i32) -> i32 {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return -1;
        }
        let p = self.px[y as usize * self.w + x as usize];
        if p >= REGION0 {
            return i32::from(p - REGION0);
        }
        if p == WHITE || self.regions.len() >= MAX_REGION {
            return -1;
        }
        let id = self.regions.len();
        let mut count = Count(0);
        self.fill(x, y, p, REGION0 + id as u16, &mut count);
        self.regions.push(Region { seed: Point { x, y }, count: count.0, capstone: -1 });
        id as i32
    }

    fn label(&self, r: usize) -> u16 {
        REGION0 + r as u16
    }

    fn find_region_corners(&mut self, r: usize, reference: Point) -> [Point; 4] {
        let seed = self.regions[r].seed;
        let mut one = OneCorner { reference, best: -1, corner: Point::default() };
        let label = self.label(r);
        self.fill(seed.x, seed.y, label, BLACK, &mut one);
        let rf = Point { x: one.corner.x - reference.x, y: one.corner.y - reference.y };
        let i = i64::from(seed.x) * i64::from(rf.x) + i64::from(seed.y) * i64::from(rf.y);
        let j = i64::from(seed.x) * -i64::from(rf.y) + i64::from(seed.y) * i64::from(rf.x);
        let mut other = OtherCorners { reference: rf, scores: [i, j, -i, -j], corners: [seed; 4] };
        self.fill(seed.x, seed.y, BLACK, label, &mut other);
        other.corners
    }

    fn record_capstone(&mut self, ring: usize, stone: usize) {
        if self.caps.len() >= MAX_CAPSTONES {
            return;
        }
        let idx = self.caps.len() as i32;
        self.regions[stone].capstone = idx;
        self.regions[ring].capstone = idx;
        let corners = self.find_region_corners(ring, self.regions[stone].seed);
        let c = perspective_setup(&corners, 7.0, 7.0);
        let center = perspective_map(&c, 3.5, 3.5);
        self.caps.push(Capstone { corners, center, c, qr_grid: -1 });
    }

    fn test_capstone(&mut self, x: i32, y: i32, pb: &[i32; 5]) {
        let ring_right = self.region_code(x - pb[4], y);
        let stone = self.region_code(x - pb[4] - pb[3] - pb[2], y);
        let ring_left = self.region_code(x - pb[4] - pb[3] - pb[2] - pb[1] - pb[0], y);
        if ring_left < 0 || ring_right < 0 || stone < 0 || ring_left != ring_right || ring_left == stone {
            return;
        }
        let (ring, stone) = (ring_left as usize, stone as usize);
        if self.regions[stone].capstone >= 0 || self.regions[ring].capstone >= 0 {
            return;
        }
        let ratio = self.regions[stone].count * 100 / self.regions[ring].count.max(1);
        if !(10..=70).contains(&ratio) {
            return;
        }
        self.record_capstone(ring, stone);
    }

    fn finder_scan(&mut self, y: usize) {
        let w = self.w;
        let mut last = false;
        let mut run = 0i32;
        let mut runs = 0;
        let mut pb = [0i32; 5];
        for x in 0..w {
            let color = self.px[y * w + x] != WHITE;
            if x > 0 && color != last {
                pb.copy_within(1..5, 0);
                pb[4] = run;
                run = 0;
                runs += 1;
                if !color && runs >= 5 {
                    let check = [1, 1, 3, 1, 1];
                    let avg = (pb[0] + pb[1] + pb[3] + pb[4]) / 4;
                    let err = avg * 3 / 4;
                    if (0..5).all(|i| pb[i] >= check[i] * avg - err && pb[i] <= check[i] * avg + err) {
                        self.test_capstone(x as i32, y as i32, &pb);
                    }
                }
            }
            run += 1;
            last = color;
        }
    }

    fn rotate_capstone(&mut self, ci: usize, h0: Point, hd: Point) {
        let cap = &mut self.caps[ci];
        let mut best = 0;
        let mut best_score = i64::MAX;
        for j in 0..4 {
            let p = cap.corners[j];
            let score = i64::from(p.x - h0.x) * -i64::from(hd.y) + i64::from(p.y - h0.y) * i64::from(hd.x);
            if j == 0 || score < best_score {
                best = j;
                best_score = score;
            }
        }
        let copy = cap.corners;
        for j in 0..4 {
            cap.corners[j] = copy[(j + best) % 4];
        }
        cap.c = perspective_setup(&cap.corners, 7.0, 7.0);
    }

    fn pixel(&self, p: Point) -> Option<bool> {
        if p.x < 0 || p.y < 0 || p.x >= self.w as i32 || p.y >= self.h as i32 {
            return None;
        }
        Some(self.px[p.y as usize * self.w + p.x as usize] != WHITE)
    }

    fn timing_scan(&self, p0: Point, p1: Point) -> i32 {
        if self.pixel(p0).is_none() || self.pixel(p1).is_none() {
            return -1;
        }
        let mut n = p1.x - p0.x;
        let mut d = p1.y - p0.y;
        let (mut x, mut y) = (p0.x, p0.y);
        let x_dominant = n.abs() > d.abs();
        if x_dominant {
            std::mem::swap(&mut n, &mut d);
        }
        let nondom_step = if n < 0 { -1 } else { 1 };
        let dom_step = if d < 0 { -1 } else { 1 };
        let (n, d) = (n.abs(), d.abs());
        let mut a = 0;
        let mut run = 0;
        let mut count = 0;
        for _ in 0..=d {
            let Some(px) = self.pixel(Point { x, y }) else { break };
            if px {
                if run >= 2 {
                    count += 1;
                }
                run = 0;
            } else {
                run += 1;
            }
            a += n;
            if x_dominant { x += dom_step } else { y += dom_step }
            if a >= d {
                if x_dominant { y += nondom_step } else { x += nondom_step }
                a -= d;
            }
        }
        count
    }

    fn measure_timing_pattern(&mut self, gi: usize) -> bool {
        let us = [6.5, 6.5, 0.5];
        let vs = [0.5, 6.5, 6.5];
        for i in 0..3 {
            let cap = &self.caps[self.grids[gi].caps[i]];
            self.grids[gi].tpep[i] = perspective_map(&cap.c, us[i], vs[i]);
        }
        let t = self.grids[gi].tpep;
        let hscan = self.timing_scan(t[1], t[2]);
        let vscan = self.timing_scan(t[1], t[0]);
        let scan = hscan.max(vscan);
        if scan < 0 {
            return false;
        }
        let size = scan * 2 + 13;
        let ver = (size - 15) / 4;
        if !(1..=MAX_VERSION).contains(&ver) {
            return false;
        }
        self.grids[gi].grid_size = ver * 4 + 17;
        true
    }

    fn find_alignment_pattern(&mut self, gi: usize) {
        let c0 = self.caps[self.grids[gi].caps[0]].c;
        let c2 = self.caps[self.grids[gi].caps[2]].c;
        let mut b = self.grids[gi].align;
        let (u, v) = perspective_unmap(&c0, b);
        let a = perspective_map(&c0, u, v + 1.0);
        let (u, v) = perspective_unmap(&c2, b);
        let c = perspective_map(&c2, u + 1.0, v);
        let size_estimate = (i64::from(a.x - b.x) * -i64::from(c.y - b.y) + i64::from(a.y - b.y) * i64::from(c.x - b.x)).abs();
        let (dx, dy) = ([1, 0, -1, 0], [0, -1, 0, 1]);
        let mut step = 1i64;
        let mut dir = 0;
        while step * step < size_estimate * 100 {
            for _ in 0..step {
                let code = self.region_code(b.x, b.y);
                if code >= 0 {
                    let count = self.regions[code as usize].count;
                    if count >= size_estimate / 2 && count <= size_estimate * 2 {
                        self.grids[gi].align_region = code;
                        return;
                    }
                }
                b.x += dx[dir];
                b.y += dy[dir];
            }
            dir = (dir + 1) % 4;
            if dir & 1 == 0 {
                step += 1;
            }
        }
    }

    fn record_qr_grid(&mut self, mut a: usize, b: usize, mut c: usize) {
        if self.grids.len() >= MAX_GRIDS {
            return;
        }
        let h0 = self.caps[a].center;
        let mut hd = Point { x: self.caps[c].center.x - h0.x, y: self.caps[c].center.y - h0.y };
        let bc = self.caps[b].center;
        if i64::from(bc.x - h0.x) * -i64::from(hd.y) + i64::from(bc.y - h0.y) * i64::from(hd.x) > 0 {
            std::mem::swap(&mut a, &mut c);
            hd = Point { x: -hd.x, y: -hd.y };
        }
        let gi = self.grids.len();
        self.grids.push(Grid { caps: [a, b, c], align_region: -1, align: Point::default(), tpep: [Point::default(); 3], grid_size: 0, c: [0.0; 8] });
        for i in 0..3 {
            let ci = self.grids[gi].caps[i];
            self.rotate_capstone(ci, h0, hd);
            self.caps[ci].qr_grid = gi as i32;
        }
        let ok = self.measure_timing_pattern(gi)
            && match line_intersect(self.caps[a].corners[0], self.caps[a].corners[1], self.caps[c].corners[0], self.caps[c].corners[3]) {
                Some(p) => {
                    self.grids[gi].align = p;
                    true
                }
                None => false,
            };
        if !ok {
            for i in 0..3 {
                let ci = self.grids[gi].caps[i];
                self.caps[ci].qr_grid = -1;
            }
            self.grids.pop();
            return;
        }
        if self.grids[gi].grid_size > 21 {
            self.find_alignment_pattern(gi);
            let ar = self.grids[gi].align_region;
            if ar >= 0 {
                let seed = self.regions[ar as usize].seed;
                let label = self.label(ar as usize);
                let best = -i64::from(hd.y) * i64::from(seed.x) + i64::from(hd.x) * i64::from(seed.y);
                let mut lm = Leftmost { reference: hd, best, corner: seed };
                self.fill(seed.x, seed.y, label, BLACK, &mut NoSpans);
                self.fill(seed.x, seed.y, BLACK, label, &mut lm);
                self.grids[gi].align = lm.corner;
            }
        }
        self.setup_qr_perspective(gi);
    }

    fn setup_qr_perspective(&mut self, gi: usize) {
        let g = &self.grids[gi];
        let rect = [self.caps[g.caps[1]].corners[0], self.caps[g.caps[2]].corners[0], g.align, self.caps[g.caps[0]].corners[0]];
        let size = f64::from(g.grid_size - 7);
        self.grids[gi].c = perspective_setup(&rect, size, size);
        self.jiggle_perspective(gi);
    }

    fn fitness_cell(&self, c: &[f64; 8], x: i32, y: i32) -> i32 {
        let offsets = [0.3, 0.5, 0.7];
        let mut score = 0;
        for v in offsets {
            for u in offsets {
                let p = perspective_map(c, f64::from(x) + u, f64::from(y) + v);
                match self.pixel(p) {
                    Some(true) => score += 1,
                    Some(false) => score -= 1,
                    None => {}
                }
            }
        }
        score
    }

    fn fitness_ring(&self, c: &[f64; 8], cx: i32, cy: i32, r: i32) -> i32 {
        let mut score = 0;
        for i in 0..r * 2 {
            score += self.fitness_cell(c, cx - r + i, cy - r);
            score += self.fitness_cell(c, cx - r, cy + r - i);
            score += self.fitness_cell(c, cx + r, cy - r + i);
            score += self.fitness_cell(c, cx + r - i, cy + r);
        }
        score
    }

    fn fitness_apat(&self, c: &[f64; 8], cx: i32, cy: i32) -> i32 {
        self.fitness_cell(c, cx, cy) - self.fitness_ring(c, cx, cy, 1) + self.fitness_ring(c, cx, cy, 2)
    }

    fn fitness_capstone(&self, c: &[f64; 8], x: i32, y: i32) -> i32 {
        let (x, y) = (x + 3, y + 3);
        self.fitness_cell(c, x, y) + self.fitness_ring(c, x, y, 1) - self.fitness_ring(c, x, y, 2) + self.fitness_ring(c, x, y, 3)
    }

    fn fitness_all(&self, gi: usize, c: &[f64; 8]) -> i32 {
        let size = self.grids[gi].grid_size;
        let version = (size - 17) / 4;
        let mut score = 0;
        for i in 0..size - 14 {
            let expect = if i & 1 == 1 { 1 } else { -1 };
            score += self.fitness_cell(c, i + 7, 6) * expect;
            score += self.fitness_cell(c, 6, i + 7) * expect;
        }
        score += self.fitness_capstone(c, 0, 0);
        score += self.fitness_capstone(c, size - 7, 0);
        score += self.fitness_capstone(c, 0, size - 7);
        if !(1..=MAX_VERSION).contains(&version) {
            return score;
        }
        let ap = VERSIONS[(version - 1) as usize].apat;
        let n = ap.len();
        for i in 1..n.saturating_sub(1) {
            score += self.fitness_apat(c, 6, i32::from(ap[i]));
            score += self.fitness_apat(c, i32::from(ap[i]), 6);
        }
        for i in 1..n {
            for j in 1..n {
                score += self.fitness_apat(c, i32::from(ap[i]), i32::from(ap[j]));
            }
        }
        score
    }

    fn jiggle_perspective(&mut self, gi: usize) {
        let mut c = self.grids[gi].c;
        let mut best = self.fitness_all(gi, &c);
        let mut adj: [f64; 8] = std::array::from_fn(|i| c[i] * 0.02);
        for _ in 0..5 {
            for i in 0..16 {
                let j = i >> 1;
                let old = c[j];
                c[j] = if i & 1 == 1 { old + adj[j] } else { old - adj[j] };
                let test = self.fitness_all(gi, &c);
                if test > best {
                    best = test;
                } else {
                    c[j] = old;
                }
            }
            adj.iter_mut().for_each(|a| *a *= 0.5);
        }
        self.grids[gi].c = c;
    }

    fn test_grouping(&mut self, i: usize) {
        if self.caps[i].qr_grid >= 0 {
            return;
        }
        let c1 = self.caps[i].c;
        let mut hlist = Vec::new();
        let mut vlist = Vec::new();
        for j in 0..self.caps.len() {
            if i == j || self.caps[j].qr_grid >= 0 {
                continue;
            }
            let (u, v) = perspective_unmap(&c1, self.caps[j].center);
            let (u, v) = ((u - 3.5).abs(), (v - 3.5).abs());
            if u < 0.2 * v {
                hlist.push((j, v));
            }
            if v < 0.2 * u {
                vlist.push((j, u));
            }
        }
        let mut best: Option<(f64, usize, usize)> = None;
        for &(hj, hd) in &hlist {
            for &(vj, vd) in &vlist {
                let score = (1.0 - hd / vd).abs();
                if score > 2.5 {
                    continue;
                }
                if best.is_none_or(|b| score < b.0) {
                    best = Some((score, hj, vj));
                }
            }
        }
        if let Some((_, h, v)) = best {
            self.record_qr_grid(h, i, v);
        }
    }

    pub fn identify(&mut self, deadline: &Deadline) -> Result<()> {
        for y in 0..self.h {
            if y % 128 == 0 {
                deadline.check()?;
            }
            self.finder_scan(y);
        }
        for i in 0..self.caps.len() {
            deadline.check()?;
            self.test_grouping(i);
        }
        Ok(())
    }

    pub fn extract(&self, gi: usize) -> Code {
        let g = &self.grids[gi];
        let n = g.grid_size as usize;
        let mut cells = vec![false; n * n];
        for y in 0..n {
            for x in 0..n {
                let p = perspective_map(&g.c, x as f64 + 0.5, y as f64 + 0.5);
                cells[y * n + x] = self.pixel(p).unwrap_or(false);
            }
        }
        Code { size: n, cells }
    }
}
