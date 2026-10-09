//! Chopping blobs along vertical lines (`fpchop.cpp`): used to split
//! underlines away from the text above them and to cut fixed-pitch rows
//! into character cells. Outline lists keep `ELIST` order through an
//! outline arena.

use super::elist::{EList, Iter};
use super::geom::ICoord;
use super::outline::{CBlob, Outline};

const MAX_OUTLINE_LENGTH: usize = 16000;

/// Owns the outlines referenced by the chopping lists.
#[derive(Default)]
pub struct OutlineArena {
    v: Vec<Option<Outline>>,
}

impl OutlineArena {
    pub fn add(&mut self, o: Outline) -> u32 {
        self.v.push(Some(o));
        (self.v.len() - 1) as u32
    }

    fn get(&self, id: u32) -> &Outline {
        self.v[id as usize].as_ref().expect("live outline")
    }

    fn get_mut(&mut self, id: u32) -> &mut Outline {
        self.v[id as usize].as_mut().expect("live outline")
    }

    fn take(&mut self, id: u32) -> Outline {
        self.v[id as usize].take().expect("live outline")
    }

    /// `new C_BLOB(&list)`: empties the list into a blob.
    pub fn take_blob(&mut self, list: &mut EList<u32>) -> CBlob {
        let ids = list.to_vec();
        *list = EList::new();
        CBlob::from_outlines(ids.into_iter().map(|i| self.take(i)).collect())
    }
}

/// `C_OUTLINE_FRAG`: `steps` is `None` for a tail.
struct Frag {
    start: ICoord,
    end: ICoord,
    steps: Option<Vec<u8>>,
    other_end: usize,
    ycoord: i32,
}

/// `split_to_blob`.
pub fn split_to_blob(
    blob: Option<CBlob>,
    chop_coord: i16,
    pitch_error: f32,
    left: &mut EList<u32>,
    right: &mut EList<u32>,
    arena: &mut OutlineArena,
) {
    if !right.is_empty() || blob.is_some() {
        fixed_chop_cblob(blob, chop_coord, pitch_error, left, right, arena);
    }
}

fn fixed_chop_cblob(
    blob: Option<CBlob>,
    chop_coord: i16,
    pitch_error: f32,
    left: &mut EList<u32>,
    right: &mut EList<u32>,
    arena: &mut OutlineArena,
) {
    let mut left_it = Iter::new(left);
    let mut right_it = Iter::new(right);
    if !right.is_empty() {
        let mut new_list: EList<u32> = EList::new();
        let mut new_it = Iter::new(&new_list);
        while !right.is_empty() {
            let old = right_it.extract(right);
            right_it.forward(right);
            fixed_split_coutline(
                old,
                chop_coord,
                pitch_error,
                (&mut *left, &mut left_it),
                (&mut new_list, &mut new_it),
                arena,
            );
        }
        right_it.add_list_before(right, &new_list.to_vec());
    }
    if let Some(b) = blob {
        for o in b.outlines {
            let id = arena.add(o);
            fixed_split_coutline(
                id,
                chop_coord,
                pitch_error,
                (&mut *left, &mut left_it),
                (&mut *right, &mut right_it),
                arena,
            );
        }
    }
}

type ListIt<'a> = (&'a mut EList<u32>, &'a mut Iter);

fn fixed_split_coutline(
    src: u32,
    chop_coord: i16,
    pitch_error: f32,
    left: ListIt,
    right: ListIt,
    arena: &mut OutlineArena,
) {
    let chop = i32::from(chop_coord);
    let srcbox = arena.get(src).bbox;
    let centre2 = srcbox.left + srcbox.right;
    if centre2 <= chop * 2 && (srcbox.right as f32) < chop as f32 + pitch_error {
        left.1.add_after_then_move(left.0, src);
    } else if centre2 > chop * 2 && srcbox.left as f32 > chop as f32 - pitch_error {
        right.1.add_before_stay_put(right.0, src);
    } else {
        let mut frags: Vec<Frag> = Vec::new();
        let mut left_frags: EList<usize> = EList::new();
        let mut right_frags: EList<usize> = EList::new();
        if fixed_chop_coutline(
            arena.get(src),
            chop_coord,
            pitch_error,
            &mut left_frags,
            &mut right_frags,
            &mut frags,
        ) {
            let children = std::mem::take(&mut arena.get_mut(src).children);
            let mut left_ch: EList<u32> = EList::new();
            let mut right_ch: EList<u32> = EList::new();
            for child in children {
                let cbox = child.bbox;
                if cbox.right < chop {
                    let id = arena.add(child);
                    left_ch.push_back(id);
                } else if cbox.left > chop {
                    let id = arena.add(child);
                    right_ch.push_back(id);
                } else if fixed_chop_coutline(
                    &child,
                    chop_coord,
                    0.0,
                    &mut left_frags,
                    &mut right_frags,
                    &mut frags,
                ) {
                    // Smashed into fragments.
                } else if cbox.left + cbox.right <= chop * 2 {
                    let id = arena.add(child);
                    left_ch.push_back(id);
                } else {
                    let id = arena.add(child);
                    right_ch.push_back(id);
                }
            }
            close_chopped_cfragments(
                &mut left_frags,
                &mut left_ch,
                pitch_error,
                left,
                arena,
                &mut frags,
            );
            close_chopped_cfragments(
                &mut right_frags,
                &mut right_ch,
                pitch_error,
                right,
                arena,
                &mut frags,
            );
            arena.take(src);
        } else if centre2 <= chop * 2 {
            left.1.add_after_then_move(left.0, src);
        } else {
            right.1.add_before_stay_put(right.0, src);
        }
    }
}

/// `fixed_chop_coutline`.
fn fixed_chop_coutline(
    src: &Outline,
    chop_coord: i16,
    pitch_error: f32,
    left_frags: &mut EList<usize>,
    right_frags: &mut EList<usize>,
    frags: &mut Vec<Frag>,
) -> bool {
    let chop = i32::from(chop_coord);
    let length = src.steps.len();
    let mut pos = src.start;
    let mut left_edge = pos.x;
    let mut tail_index = 0usize;
    let mut tail_pos = pos;
    for s in 0..length {
        if pos.x < left_edge {
            left_edge = pos.x;
            tail_index = s;
            tail_pos = pos;
        }
        pos += src.step(s);
    }
    if left_edge as f32 >= chop as f32 - pitch_error {
        return false;
    }
    let startindex = tail_index;
    let mut first_frag = true;
    let mut head_index = tail_index;
    let mut head_pos = tail_pos;
    let mut first_index = 0usize;
    let mut first_pos = ICoord::default();
    let advance = |idx: &mut usize, p: &mut ICoord| {
        *p += src.step(*idx);
        *idx += 1;
        if *idx == length {
            *idx = 0;
        }
    };
    loop {
        loop {
            advance(&mut tail_index, &mut tail_pos);
            if !(tail_pos.x != chop && tail_index != startindex) {
                break;
            }
        }
        if tail_index == startindex {
            if first_frag {
                return false;
            }
            break;
        }
        if !first_frag {
            save_chop_cfragment(
                head_index, head_pos, tail_index, tail_pos, src, left_frags, frags,
            );
        } else {
            first_index = tail_index;
            first_pos = tail_pos;
            first_frag = false;
        }
        while src.step(tail_index).x == 0 {
            advance(&mut tail_index, &mut tail_pos);
        }
        head_index = tail_index;
        head_pos = tail_pos;
        while src.step(tail_index).x > 0 {
            loop {
                advance(&mut tail_index, &mut tail_pos);
                if tail_pos.x == chop {
                    break;
                }
            }
            save_chop_cfragment(
                head_index,
                head_pos,
                tail_index,
                tail_pos,
                src,
                right_frags,
                frags,
            );
            while src.step(tail_index).x == 0 {
                advance(&mut tail_index, &mut tail_pos);
            }
            head_index = tail_index;
            head_pos = tail_pos;
        }
        if tail_index == startindex {
            break;
        }
    }
    save_chop_cfragment(
        head_index,
        head_pos,
        first_index,
        first_pos,
        src,
        left_frags,
        frags,
    );
    true
}

fn save_chop_cfragment(
    head_index: usize,
    head_pos: ICoord,
    tail_index: usize,
    tail_pos: ICoord,
    src: &Outline,
    list: &mut EList<usize>,
    frags: &mut Vec<Frag>,
) {
    let len = src.steps.len() as i32;
    let mut stepcount = tail_index as i32 - head_index as i32;
    if stepcount < 0 {
        stepcount += len;
    }
    let jump = (tail_pos.y - head_pos.y).abs();
    if jump == stepcount {
        return;
    }
    let steps: Vec<u8> = (0..stepcount as usize)
        .map(|i| src.steps[(head_index + i) % len as usize])
        .collect();
    let head = frags.len();
    let tail = head + 1;
    frags.push(Frag {
        start: head_pos,
        end: tail_pos,
        steps: Some(steps),
        other_end: tail,
        ycoord: head_pos.y,
    });
    frags.push(Frag {
        start: head_pos,
        end: tail_pos,
        steps: None,
        other_end: head,
        ycoord: tail_pos.y,
    });
    add_frag_to_list(head, list, frags);
    add_frag_to_list(tail, list, frags);
}

fn add_frag_to_list(f: usize, list: &mut EList<usize>, frags: &[Frag]) {
    let mut it = Iter::new(list);
    let y = frags[f].ycoord;
    if !list.is_empty() {
        it.mark_cycle_pt();
        while !it.cycled_list(list) {
            let d = &frags[it.data(list)];
            if d.ycoord > y || (d.ycoord == y && frags[frags[f].other_end].ycoord < y) {
                it.add_before_then_move(list, f);
                return;
            }
            it.forward(list);
        }
    }
    it.add_to_end(list, f);
}

fn close_chopped_cfragments(
    list: &mut EList<usize>,
    children: &mut EList<u32>,
    pitch_error: f32,
    dest: ListIt,
    arena: &mut OutlineArena,
    frags: &mut [Frag],
) {
    let mut frag_it = Iter::new(list);
    let mut child_it = Iter::new(children);
    while !list.is_empty() {
        frag_it.move_to_first(list);
        let bottom = frag_it.extract(list);
        frag_it.forward(list);
        let top = frag_it.data(list);
        if frags[bottom].steps.is_none() == frags[top].steps.is_none()
            && frags[frag_it.data_relative(list, 1)].ycoord == frags[top].ycoord
        {
            frag_it.forward(list);
        }
        let top = frag_it.extract(list);
        if frags[top].other_end != bottom {
            let o = join_chopped_fragments(bottom, top, frags);
            debug_assert!(o.is_none());
        } else if let Some(outline) = join_chopped_fragments(bottom, top, frags) {
            let oid = arena.add(outline);
            child_it.mark_cycle_pt();
            while !child_it.cycled_list(children) {
                let c = child_it.data(children);
                if arena.get(c).inside(arena.get(oid)) {
                    let e = child_it.extract(children);
                    let child = arena.take(e);
                    arena.get_mut(oid).children.push(child);
                }
                child_it.forward(children);
            }
            if arena.get(oid).bbox.width() as f32 > pitch_error {
                dest.1.add_after_then_move(dest.0, oid);
            } else {
                arena.take(oid);
            }
        }
    }
    while !children.is_empty() {
        let c = child_it.extract(children);
        dest.1.add_after_then_move(dest.0, c);
        child_it.forward(children);
    }
}

fn join_chopped_fragments(bottom: usize, top: usize, frags: &mut [Frag]) -> Option<Outline> {
    if frags[bottom].other_end == top {
        return if frags[bottom].steps.is_none() {
            close_frag(&frags[top])
        } else {
            close_frag(&frags[bottom])
        };
    }
    if frags[bottom].steps.is_none() {
        join_segments(frags[bottom].other_end, top, frags);
    } else {
        join_segments(frags[top].other_end, bottom, frags);
    }
    let te = frags[top].other_end;
    let be = frags[bottom].other_end;
    frags[te].other_end = be;
    frags[be].other_end = te;
    None
}

fn fake_steps(from_y: i32, to_y: i32) -> (u8, usize) {
    // DIR128 32 is (0,-1) = chain code 1; 96 is (0,1) = chain code 3.
    let fake_count = to_y - from_y;
    if fake_count < 0 {
        (1, (-fake_count) as usize)
    } else {
        (3, fake_count as usize)
    }
}

fn join_segments(bottom: usize, top: usize, frags: &mut [Frag]) {
    let (fake, count) = fake_steps(frags[bottom].end.y, frags[top].start.y);
    let mut steps = frags[bottom].steps.take().expect("head fragment");
    steps.extend(std::iter::repeat_n(fake, count));
    steps.extend_from_slice(frags[top].steps.as_ref().expect("head fragment"));
    frags[bottom].steps = Some(steps);
    let end = frags[top].end;
    frags[bottom].end = end;
    let oe = frags[bottom].other_end;
    frags[oe].end = end;
}

/// `C_OUTLINE_FRAG::close`.
fn close_frag(f: &Frag) -> Option<Outline> {
    let (fake, count) = fake_steps(f.end.y, f.start.y);
    let steps = f.steps.as_ref().expect("head fragment");
    if steps.len() + count > MAX_OUTLINE_LENGTH {
        return None;
    }
    let mut all = steps.clone();
    all.extend(std::iter::repeat_n(fake, count));
    Some(Outline::from_steps(f.start, &all))
}
