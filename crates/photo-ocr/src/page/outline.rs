//! Crack-edge outline extraction (`scanedg.cpp`, `edgloop.cpp`), chain-coded
//! outlines (`C_OUTLINE`), their grouping into blobs (`edgblob.cpp`) and
//! `C_BLOB` construction (`stepblob.cpp`).

use super::bitmap::Bitmap;
use super::geom::{ICoord, TBox};

/// Chain code step vectors, indexed by direction 0..4.
pub const STEP: [ICoord; 4] = [
    ICoord::new(-1, 0),
    ICoord::new(0, -1),
    ICoord::new(1, 0),
    ICoord::new(0, 1),
];

const INTERSECTING: i16 = i16::MAX;
const MAX_OUTLINE_LENGTH: i32 = 16000;
const MIN_EDGE_LENGTH: i32 = 8;
const BUCKETSIZE: i32 = 16;

const EDGES_CHILDREN_PER_GRANDCHILD: i32 = 10;
const EDGES_CHILDREN_COUNT_LIMIT: i32 = 45;
const EDGES_CHILDAREA: f64 = 0.5;
const EDGES_BOXAREA: f64 = 0.875;

#[derive(Clone, Debug)]
pub struct Outline {
    pub start: ICoord,
    /// Chain codes 0..4 (see [`STEP`]).
    pub steps: Vec<u8>,
    pub bbox: TBox,
    pub children: Vec<Outline>,
    pub inverse: bool,
}

impl Outline {
    pub fn step(&self, i: usize) -> ICoord {
        STEP[self.steps[i] as usize]
    }

    pub fn pathlength(&self) -> i32 {
        self.steps.len() as i32
    }

    pub fn outer_area(&self) -> i32 {
        if self.steps.is_empty() {
            return self.bbox.area();
        }
        let mut pos = self.start;
        let mut total = 0;
        for &s in &self.steps {
            let st = STEP[s as usize];
            if st.x < 0 {
                total += pos.y;
            } else if st.x > 0 {
                total -= pos.y;
            }
            pos += st;
        }
        total
    }

    pub fn area(&self) -> i32 {
        let mut pos = self.start;
        let mut total = 0;
        for &s in &self.steps {
            let st = STEP[s as usize];
            if st.x < 0 {
                total += pos.y;
            } else if st.x > 0 {
                total -= pos.y;
            }
            pos += st;
        }
        total + self.children.iter().map(Outline::area).sum::<i32>()
    }

    pub fn perimeter(&self) -> i32 {
        self.pathlength() + self.children.iter().map(Outline::pathlength).sum::<i32>()
    }

    pub fn winding_number(&self, point: ICoord) -> i16 {
        let mut vec = self.start - point;
        let mut count: i16 = 0;
        for &s in &self.steps {
            let st = STEP[s as usize];
            if vec.y <= 0 && vec.y + st.y > 0 {
                let cross = vec.cross(st);
                if cross > 0 {
                    count += 1;
                } else if cross == 0 {
                    return INTERSECTING;
                }
            } else if vec.y > 0 && vec.y + st.y <= 0 {
                let cross = vec.cross(st);
                if cross < 0 {
                    count -= 1;
                } else if cross == 0 {
                    return INTERSECTING;
                }
            }
            vec += st;
        }
        count
    }

    /// `operator<`: true if `self` is inside `other`.
    pub fn inside(&self, other: &Outline) -> bool {
        if !self.bbox.overlap(&other.bbox) {
            return false;
        }
        if self.steps.is_empty() {
            return other.bbox.contains(&self.bbox);
        }
        let mut pos = self.start;
        let mut count = INTERSECTING;
        for &s in &self.steps {
            count = other.winding_number(pos);
            if count != INTERSECTING {
                break;
            }
            pos += STEP[s as usize];
        }
        if count == INTERSECTING {
            let mut pos = other.start;
            for &s in &other.steps {
                count = self.winding_number(pos);
                if count != INTERSECTING {
                    break;
                }
                pos += STEP[s as usize];
            }
            return count == INTERSECTING || count == 0;
        }
        count != 0
    }

    /// Sum of direction changes in units of 32 (`DIR128`): ±128.
    pub fn turn_direction(&self) -> i32 {
        let n = self.steps.len();
        if n == 0 {
            return 128;
        }
        let mut count = 0i32;
        let mut prev = i32::from(self.steps[n - 1]) * 32;
        for &s in &self.steps {
            let dir = i32::from(s) * 32;
            // DIR128 subtraction wraps into [-64, 64).
            let mut diff = (dir - prev).rem_euclid(128);
            if diff >= 64 {
                diff -= 128;
            }
            count += diff;
            prev = dir;
        }
        count
    }

    pub fn reverse(&mut self) {
        let n = self.steps.len();
        let half = n.div_ceil(2);
        for i in 0..half {
            let far = n - i - 1;
            let a = self.steps[i];
            self.steps[i] = (self.steps[far] + 2) & 3;
            self.steps[far] = (a + 2) & 3;
        }
    }

    pub fn is_legally_nested(&self) -> bool {
        if self.steps.is_empty() {
            return true;
        }
        let parent_area = i64::from(self.outer_area());
        self.children
            .iter()
            .all(|c| i64::from(c.outer_area()) * parent_area <= 0 && c.is_legally_nested())
    }

    pub fn move_by(&mut self, v: ICoord) {
        self.bbox.move_by(v);
        self.start += v;
        for c in &mut self.children {
            c.move_by(v);
        }
    }
}

fn reverse_outline_list(list: &mut [Outline]) {
    for o in list {
        o.reverse();
        o.inverse = true;
        reverse_outline_list(&mut o.children);
    }
}

/// `C_BLOB`.
#[derive(Clone, Debug, Default)]
pub struct CBlob {
    pub outlines: Vec<Outline>,
}

impl CBlob {
    pub fn bounding_box(&self) -> TBox {
        let mut b = TBox::default();
        for o in &self.outlines {
            b.union_with(&o.bbox);
        }
        b
    }

    pub fn area(&self) -> i32 {
        self.outlines.iter().map(Outline::area).sum()
    }

    pub fn outer_area(&self) -> i32 {
        self.outlines.iter().map(Outline::outer_area).sum()
    }

    pub fn perimeter(&self) -> i32 {
        self.outlines.iter().map(Outline::perimeter).sum()
    }

    fn check_inverse_flag_and_direction(&mut self) {
        for o in &mut self.outlines {
            if o.turn_direction() < 0 {
                o.reverse();
                reverse_outline_list(&mut o.children);
                o.inverse = true;
            } else {
                o.inverse = false;
            }
        }
    }

    /// `C_BLOB(C_OUTLINE_LIST*)`: nests the outlines.
    pub fn from_outlines(list: Vec<Outline>) -> CBlob {
        let mut nested = Vec::new();
        for o in list {
            position_outline(o, &mut nested);
        }
        let mut b = CBlob { outlines: nested };
        b.check_inverse_flag_and_direction();
        b
    }

    pub fn move_by(&mut self, v: ICoord) {
        for o in &mut self.outlines {
            o.move_by(v);
        }
    }
}

/// `position_outline`.
fn position_outline(mut outline: Outline, dest: &mut Vec<Outline>) {
    let mut i = 0;
    while i < dest.len() {
        if dest[i].inside(&outline) {
            let first = std::mem::replace(&mut dest[i], Outline::placeholder());
            outline.children.push(first);
            let mut j = i + 1;
            while j < dest.len() {
                if dest[j].inside(&outline) {
                    outline.children.push(dest.remove(j));
                } else {
                    j += 1;
                }
            }
            dest[i] = outline;
            return;
        } else if outline.inside(&dest[i]) {
            position_outline(outline, &mut dest[i].children);
            return;
        }
        i += 1;
    }
    dest.push(outline);
}

impl Outline {
    fn placeholder() -> Outline {
        Outline {
            start: ICoord::default(),
            steps: Vec::new(),
            bbox: TBox::default(),
            children: Vec::new(),
            inverse: false,
        }
    }
}

/// `C_BLOB::ConstructBlobsFromOutlines`: returns (blob, good) pairs in order.
fn construct_blobs(good_blob: bool, list: Vec<Outline>, out: &mut Vec<(CBlob, bool)>) {
    let mut nested = Vec::new();
    for o in list {
        position_outline(o, &mut nested);
    }
    let mut queue: std::collections::VecDeque<Outline> = nested.into();
    while let Some(mut o) = queue.pop_front() {
        let mut good = good_blob;
        if !o.is_legally_nested() {
            good = false;
            for c in std::mem::take(&mut o.children).into_iter().rev() {
                queue.push_front(c);
            }
        }
        let mut blob = CBlob { outlines: vec![o] };
        blob.check_inverse_flag_and_direction();
        out.push((blob, good));
    }
}

// ---------------------------------------------------------------------------
// Crack edge tracing.

const NIL: u32 = u32::MAX;
const WHITE: u8 = 1;

#[derive(Clone, Copy, Default)]
struct Crack {
    x: i32,
    y: i32,
    stepx: i8,
    stepy: i8,
    dir: u8,
    next: u32,
    prev: u32,
}

struct Tracer {
    cracks: Vec<Crack>,
    free: u32,
    outlines: Vec<Outline>,
}

impl Tracer {
    fn alloc(&mut self) -> u32 {
        if self.free != NIL {
            let n = self.free;
            self.free = self.cracks[n as usize].next;
            n
        } else {
            self.cracks.push(Crack::default());
            (self.cracks.len() - 1) as u32
        }
    }

    fn link(&mut self, n: u32, join: u32, joins_before: bool) {
        let c = &mut self.cracks;
        if join == NIL {
            c[n as usize].next = n;
            c[n as usize].prev = n;
        } else if joins_before {
            let jp = c[join as usize].prev;
            c[n as usize].prev = jp;
            c[jp as usize].next = n;
            c[n as usize].next = join;
            c[join as usize].prev = n;
        } else {
            let jn = c[join as usize].next;
            c[n as usize].next = jn;
            c[jn as usize].prev = n;
            c[n as usize].prev = join;
            c[join as usize].next = n;
        }
    }

    fn h_edge(&mut self, sign: i32, join: u32, x: i32, y: i32) -> u32 {
        let n = self.alloc();
        let (px, stepx, dir) = if sign > 0 { (x + 1, -1, 0) } else { (x, 1, 2) };
        self.cracks[n as usize] = Crack {
            x: px,
            y: y + 1,
            stepx,
            stepy: 0,
            dir,
            next: NIL,
            prev: NIL,
        };
        let before = join != NIL && {
            let j = self.cracks[join as usize];
            px + i32::from(stepx) == j.x && y + 1 == j.y
        };
        self.link(n, join, before);
        n
    }

    fn v_edge(&mut self, sign: i32, join: u32, x: i32, y: i32) -> u32 {
        let n = self.alloc();
        let (py, stepy, dir) = if sign > 0 { (y, 1, 3) } else { (y + 1, -1, 1) };
        self.cracks[n as usize] = Crack {
            x,
            y: py,
            stepx: 0,
            stepy,
            dir,
            next: NIL,
            prev: NIL,
        };
        let before = join != NIL && {
            let j = self.cracks[join as usize];
            x == j.x && py + i32::from(stepy) == j.y
        };
        self.link(n, join, before);
        n
    }

    fn join_edges(&mut self, mut e1: u32, mut e2: u32) {
        {
            let a = self.cracks[e1 as usize];
            let b = self.cracks[e2 as usize];
            if a.x + i32::from(a.stepx) != b.x || a.y + i32::from(a.stepy) != b.y {
                std::mem::swap(&mut e1, &mut e2);
            }
        }
        if self.cracks[e1 as usize].next == e2 {
            self.complete_edge(e1);
            let p = self.cracks[e1 as usize].prev;
            self.cracks[p as usize].next = self.free;
            self.free = e1;
        } else {
            let e2p = self.cracks[e2 as usize].prev;
            let e1n = self.cracks[e1 as usize].next;
            self.cracks[e2p as usize].next = e1n;
            self.cracks[e1n as usize].prev = e2p;
            self.cracks[e1 as usize].next = e2;
            self.cracks[e2 as usize].prev = e1;
        }
    }

    fn complete_edge(&mut self, start: u32) {
        let c = &self.cracks;
        // check_path_legal
        let mut length = 0;
        let mut chainsum = 0;
        let mut e = start;
        let mut lastchain = i32::from(c[c[e as usize].prev as usize].dir);
        loop {
            length += 1;
            let d = i32::from(c[e as usize].dir);
            if d != lastchain {
                let mut diff = d - lastchain;
                if diff > 2 {
                    diff -= 4;
                } else if diff < -2 {
                    diff += 4;
                }
                chainsum += diff;
                lastchain = d;
            }
            e = c[e as usize].next;
            if e == start || length >= MAX_OUTLINE_LENGTH {
                break;
            }
        }
        if (chainsum != 4 && chainsum != -4) || e != start || length < MIN_EDGE_LENGTH {
            return;
        }
        // loop_bounding_box
        let s = c[start as usize];
        let (mut bl, mut tr) = (ICoord::new(s.x, s.y), ICoord::new(s.x, s.y));
        let mut leftmost = s.x;
        let mut realstart = start;
        let mut len = 0usize;
        let mut e = start;
        loop {
            e = c[e as usize].next;
            let p = c[e as usize];
            if p.x < bl.x {
                bl.x = p.x;
            } else if p.x > tr.x {
                tr.x = p.x;
            }
            if p.y < bl.y {
                bl.y = p.y;
            } else if p.y > tr.y {
                realstart = e;
                leftmost = p.x;
                tr.y = p.y;
            } else if p.y == tr.y && p.x < leftmost {
                leftmost = p.x;
                realstart = e;
            }
            len += 1;
            if e == start {
                break;
            }
        }
        let mut steps = Vec::with_capacity(len);
        let mut e = realstart;
        for _ in 0..len {
            steps.push(c[e as usize].dir);
            e = c[e as usize].next;
        }
        let rs = c[realstart as usize];
        self.outlines.push(Outline {
            start: ICoord::new(rs.x, rs.y),
            steps,
            bbox: TBox::from_corners(bl, tr),
            children: Vec::new(),
            inverse: false,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn line_edges(
        &mut self,
        y: i32,
        xext: i32,
        mut uppercolour: u8,
        bw: &[u8],
        prevline: &mut [u32],
    ) {
        let mut prevcolour = uppercolour;
        let mut current = NIL;
        for x in 0..xext {
            let colour = bw[x as usize];
            let pl = &mut prevline[x as usize];
            if *pl != NIL {
                uppercolour = 1 - uppercolour;
                if colour == prevcolour {
                    if colour == uppercolour {
                        let p = *pl;
                        self.join_edges(current, p);
                        current = NIL;
                    } else {
                        let p = *pl;
                        current = self.h_edge(i32::from(uppercolour) - i32::from(colour), p, x, y);
                    }
                    prevline[x as usize] = NIL;
                } else {
                    let p = *pl;
                    if colour == uppercolour {
                        prevline[x as usize] =
                            self.v_edge(i32::from(colour) - i32::from(prevcolour), p, x, y);
                    } else if colour == WHITE {
                        self.join_edges(current, p);
                        current =
                            self.h_edge(i32::from(uppercolour) - i32::from(colour), NIL, x, y);
                        prevline[x as usize] =
                            self.v_edge(i32::from(colour) - i32::from(prevcolour), current, x, y);
                    } else {
                        let nc = self.h_edge(i32::from(uppercolour) - i32::from(colour), p, x, y);
                        prevline[x as usize] =
                            self.v_edge(i32::from(colour) - i32::from(prevcolour), current, x, y);
                        current = nc;
                    }
                    prevcolour = colour;
                }
            } else {
                if colour != prevcolour {
                    current = self.v_edge(i32::from(colour) - i32::from(prevcolour), current, x, y);
                    prevline[x as usize] = current;
                    prevcolour = colour;
                }
                if colour != uppercolour {
                    current =
                        self.h_edge(i32::from(uppercolour) - i32::from(colour), current, x, y);
                } else {
                    current = NIL;
                }
            }
        }
        let x = xext;
        let pl = prevline[x as usize];
        let flip = 1 - i32::from(prevcolour);
        if current != NIL {
            if pl != NIL {
                self.join_edges(current, pl);
                prevline[x as usize] = NIL;
            } else {
                prevline[x as usize] = self.v_edge(flip - i32::from(prevcolour), current, x, y);
            }
        } else if pl != NIL {
            prevline[x as usize] = self.v_edge(flip - i32::from(prevcolour), pl, x, y);
        }
    }
}

/// `block_edges` over the whole image: closed outlines in completion order,
/// in page coordinates (y up).
pub fn trace_outlines(pix: &Bitmap) -> Vec<Outline> {
    let (w, h) = (pix.width as i32, pix.height as i32);
    let mut t = Tracer {
        cracks: Vec::new(),
        free: NIL,
        outlines: Vec::new(),
    };
    let mut prevline = vec![NIL; w as usize + 1];
    let mut bw = vec![WHITE; w as usize];
    for y in (-1..h).rev() {
        if y >= 0 {
            let row = pix.row((h - 1 - y) as usize);
            for (x, v) in bw.iter_mut().enumerate() {
                *v = (((row[x >> 5] >> (31 - (x & 31))) & 1) ^ 1) as u8;
            }
        } else {
            bw.fill(WHITE);
        }
        t.line_edges(y, w, WHITE, &bw, &mut prevline);
    }
    t.outlines
}

// ---------------------------------------------------------------------------
// OL_BUCKETS.

struct Buckets {
    bxdim: i32,
    lists: Vec<Vec<usize>>,
    /// Index of the bucket the scan is at.
    it: usize,
}

impl Buckets {
    fn index(&self, x: i32, y: i32) -> usize {
        ((y / BUCKETSIZE) * self.bxdim + x / BUCKETSIZE) as usize
    }

    fn range(&self, b: &TBox) -> (i32, i32, i32, i32) {
        (
            b.left / BUCKETSIZE,
            b.right / BUCKETSIZE,
            b.bottom / BUCKETSIZE,
            b.top / BUCKETSIZE,
        )
    }

    fn count_children(&self, arena: &[Option<Outline>], o: usize, max_count: i32) -> i32 {
        let outline = arena[o].as_ref().expect("live outline");
        let (xmin, xmax, ymin, ymax) = self.range(&outline.bbox);
        let mut child_count = 0;
        let mut grandchild_count = 0;
        let mut parent_area = 0;
        let mut parent_box = true;
        for y in ymin..=ymax {
            for x in xmin..=xmax {
                for &c in &self.lists[(y * self.bxdim + x) as usize] {
                    let child = arena[c].as_ref().expect("live outline");
                    if c == o || !child.inside(outline) {
                        continue;
                    }
                    child_count += 1;
                    if child_count <= max_count {
                        let max_grand = (max_count - child_count) / EDGES_CHILDREN_PER_GRANDCHILD;
                        if max_grand > 0 {
                            grandchild_count += self.count_children(arena, c, max_grand)
                                * EDGES_CHILDREN_PER_GRANDCHILD;
                        } else {
                            grandchild_count += self.count_children(arena, c, 1);
                        }
                    }
                    if child_count + grandchild_count > max_count {
                        return child_count + grandchild_count;
                    }
                    if parent_area == 0 {
                        parent_area = outline.outer_area().abs();
                        let max_parent_area =
                            (f64::from(outline.bbox.area()) * EDGES_BOXAREA) as f32;
                        if (parent_area as f32) < max_parent_area {
                            parent_box = false;
                        }
                    }
                    if parent_box {
                        let child_area = child.outer_area().abs();
                        if f64::from(child_area) < f64::from(child.bbox.area()) * EDGES_CHILDAREA {
                            return max_count + 1;
                        }
                    }
                }
            }
        }
        child_count + grandchild_count
    }

    fn extract_children(&mut self, arena: &mut [Option<Outline>], o: usize) -> Vec<Outline> {
        let outline = arena[o].take().expect("live outline");
        let (xmin, xmax, ymin, ymax) = self.range(&outline.bbox);
        let mut out = Vec::new();
        for y in ymin..=ymax {
            for x in xmin..=xmax {
                let list = &mut self.lists[(y * self.bxdim + x) as usize];
                let mut i = 0;
                while i < list.len() {
                    let c = list[i];
                    if arena[c].as_ref().expect("live outline").inside(&outline) {
                        list.remove(i);
                        out.push(arena[c].take().expect("live outline"));
                    } else {
                        i += 1;
                    }
                }
            }
        }
        arena[o] = Some(outline);
        out
    }
}

/// `outlines_to_blobs` for a block covering the whole page: returns the
/// good blobs and the rejected blobs, each in Tesseract's order.
pub fn outlines_to_blobs(
    width: i32,
    height: i32,
    outlines: Vec<Outline>,
) -> (Vec<CBlob>, Vec<CBlob>) {
    let bxdim = width / BUCKETSIZE + 1;
    let bydim = height / BUCKETSIZE + 1;
    let mut b = Buckets {
        bxdim,
        lists: vec![Vec::new(); (bxdim * bydim) as usize],
        it: 0,
    };
    let mut arena: Vec<Option<Outline>> = Vec::with_capacity(outlines.len());
    for o in outlines {
        let idx = b.index(o.bbox.left, o.bbox.bottom);
        b.lists[idx].push(arena.len());
        arena.push(Some(o));
    }
    let mut good = Vec::new();
    let mut bad = Vec::new();
    let mut made = Vec::new();
    while let Some(i) = b.lists[b.it..].iter().position(|l| !l.is_empty()) {
        b.it += i;
        let list = &b.lists[b.it];
        let mut p = 0;
        for j in 1..list.len() {
            let parent = arena[list[p]].as_ref().expect("live outline");
            if parent.inside(arena[list[j]].as_ref().expect("live outline")) {
                p = j;
            }
        }
        let parent = b.lists[b.it].remove(p);
        let count = b.count_children(&arena, parent, EDGES_CHILDREN_COUNT_LIMIT);
        let ok = count <= EDGES_CHILDREN_COUNT_LIMIT;
        let mut list = Vec::new();
        let children = if ok && count > 0 {
            b.extract_children(&mut arena, parent)
        } else {
            Vec::new()
        };
        list.push(arena[parent].take().expect("live outline"));
        list.extend(children);
        construct_blobs(ok, list, &mut made);
        for (blob, g) in made.drain(..) {
            if g { good.push(blob) } else { bad.push(blob) }
        }
    }
    (good, bad)
}

/// `find_cblob_limits` / `find_cblob_vlimits` without rotation: the y range
/// of outline points with `leftx <= x <= rightx`, as (ymin, ymax).
pub fn find_cblob_limits(blob: &CBlob, leftx: f32, rightx: f32) -> (f32, f32) {
    let mut ymin = i32::MAX as f32;
    let mut ymax = -i32::MAX as f32;
    for o in &blob.outlines {
        let mut pos = o.start;
        for &s in &o.steps {
            if pos.x as f32 >= leftx && pos.x as f32 <= rightx {
                let y = pos.y as f32;
                if y < ymin {
                    ymin = y;
                }
                if y > ymax {
                    ymax = y;
                }
            }
            pos += STEP[s as usize];
        }
    }
    (ymin, ymax)
}

/// `find_cblob_hlimits`: the x range of outline points with
/// `bottomy <= y <= topy`, as (xmin, xmax).
pub fn find_cblob_hlimits(blob: &CBlob, bottomy: f32, topy: f32) -> (f32, f32) {
    let mut xmin = i32::MAX as f32;
    let mut xmax = -i32::MAX as f32;
    for o in &blob.outlines {
        let mut pos = o.start;
        for &s in &o.steps {
            if pos.y as f32 >= bottomy && pos.y as f32 <= topy {
                let x = pos.x as f32;
                if x < xmin {
                    xmin = x;
                }
                if x > xmax {
                    xmax = x;
                }
            }
            pos += STEP[s as usize];
        }
    }
    (xmin, xmax)
}

/// `C_BLOB::render_outline`: the top-level outline pixels only.
pub fn render_outline(blob: &CBlob) -> Bitmap {
    let b = blob.bounding_box();
    let mut pix = Bitmap::new(b.width().max(0) as usize, b.height().max(0) as usize);
    let (left, top) = (b.left, b.top);
    let mut set = |x: i32, y: i32| {
        if x >= 0 && y >= 0 && (x as usize) < pix.width && (y as usize) < pix.height {
            pix.set(x as usize, y as usize);
        }
    };
    for o in &blob.outlines {
        let mut pos = o.start;
        for &s in &o.steps {
            let st = STEP[s as usize];
            if st.y < 0 {
                set(pos.x - left, top - pos.y);
            } else if st.y > 0 {
                set(pos.x - left - 1, top - pos.y - 1);
            } else if st.x < 0 {
                set(pos.x - left - 1, top - pos.y);
            } else if st.x > 0 {
                set(pos.x - left, top - pos.y - 1);
            }
            pos += st;
        }
    }
    pix
}
