//! Dictionary: `SquishedDawg` and the parts of `Dict` the LSTM beam search
//! uses (`default_dawgs`, `def_letter_is_okay`).

use crate::reader::Reader;
use crate::unicharset::{INVALID_UNICHAR_ID, Unicharset};
use crate::{Error, Result};

pub(crate) const NO_EDGE: i64 = -1;
const PATTERN_UNICHAR_ID: i32 = 0;

// PermuterType values used here.
pub(crate) const NO_PERM: i32 = 0;
pub(crate) const PUNC_PERM: i32 = 1;
pub(crate) const TOP_CHOICE_PERM: i32 = 2;
pub(crate) const NUMBER_PERM: i32 = 6;
pub(crate) const SYSTEM_DAWG_PERM: i32 = 8;
pub(crate) const COMPOUND_PERM: i32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DawgType {
    Punctuation,
    Word,
    Number,
}

/// `kDawgSuccessors[from][to]`.
fn successor(from: DawgType, to: DawgType) -> bool {
    matches!(
        (from, to),
        (DawgType::Punctuation, DawgType::Word)
            | (DawgType::Punctuation, DawgType::Number)
            | (DawgType::Word, DawgType::Punctuation)
            | (DawgType::Number, DawgType::Punctuation)
    )
}

pub(crate) struct Dawg {
    pub(crate) kind: DawgType,
    pub(crate) perm: i32,
    edges: Vec<u64>,
    flag_start_bit: u32,
    next_node_start_bit: u32,
    letter_mask: u64,
    next_node_mask: u64,
    node0_forward: i64,
}

const MARKER_FLAG: u64 = 1;
const DIRECTION_FLAG: u64 = 2;
const WERD_END_FLAG: u64 = 4;

impl Dawg {
    pub(crate) fn read(data: &[u8], kind: DawgType, perm: i32) -> Result<Dawg> {
        let mut r = Reader::new(data);
        if r.i16()? != 42 {
            return Err(Error::Model("bad dawg magic"));
        }
        let unicharset_size = r.i32()?;
        let num_edges = r.i32()?;
        if unicharset_size <= 0 || num_edges <= 0 || num_edges > 50_000_000 {
            return Err(Error::Model("bad dawg size"));
        }
        let mut edges = Vec::with_capacity(num_edges as usize);
        for _ in 0..num_edges {
            edges.push(r.u64()?);
        }
        let flag_start_bit = ((f64::from(unicharset_size) + 1.0).ln() / 2.0f64.ln()).ceil() as u32;
        let next_node_start_bit = flag_start_bit + 3;
        let mut d = Dawg {
            kind,
            perm,
            edges,
            flag_start_bit,
            next_node_start_bit,
            letter_mask: !(!0u64 << flag_start_bit),
            next_node_mask: !0u64 << (flag_start_bit + 3),
            node0_forward: 0,
        };
        d.node0_forward = d.num_forward_edges(0);
        Ok(d)
    }

    fn rec(&self, e: i64) -> u64 {
        self.edges[e as usize]
    }

    fn occupied(&self, e: i64) -> bool {
        self.rec(e) != self.next_node_mask
    }

    fn last_edge(&self, e: i64) -> bool {
        self.rec(e) & (MARKER_FLAG << self.flag_start_bit) != 0
    }

    fn forward(&self, e: i64) -> bool {
        self.occupied(e) && self.rec(e) & (DIRECTION_FLAG << self.flag_start_bit) == 0
    }

    fn letter(&self, rec: u64) -> i32 {
        (rec & self.letter_mask) as i32
    }

    fn eow(&self, rec: u64) -> bool {
        rec & (WERD_END_FLAG << self.flag_start_bit) != 0
    }

    fn num_forward_edges(&self, node: i64) -> i64 {
        let mut n = 0;
        let mut e = node;
        if (e as usize) < self.edges.len() && self.forward(e) {
            loop {
                n += 1;
                let last = self.last_edge(e);
                e += 1;
                if last || e as usize >= self.edges.len() {
                    break;
                }
            }
        }
        n
    }

    pub(crate) fn next_node(&self, e: i64) -> i64 {
        ((self.rec(e) & self.next_node_mask) >> self.next_node_start_bit) as i64
    }

    pub(crate) fn end_of_word(&self, e: i64) -> bool {
        self.eow(self.rec(e))
    }

    /// `given_greater_than_edge_rec` with `next_node = NO_EDGE` (which
    /// matches any next node and is below every real one).
    fn compare(&self, word_end: bool, unichar_id: i32, rec: u64) -> i32 {
        let cur_id = self.letter(rec);
        if unichar_id == cur_id && (!word_end || self.eow(rec)) {
            return 0;
        }
        if unichar_id > cur_id { 1 } else { -1 }
    }

    pub(crate) fn edge_char_of(&self, node: i64, unichar_id: i32, word_end: bool) -> i64 {
        if node == 0 {
            let (mut start, mut end) = (0i64, self.node0_forward - 1);
            while start <= end {
                let edge = (start + end) >> 1;
                match self.compare(word_end, unichar_id, self.rec(edge)) {
                    0 => return edge,
                    1 => start = edge + 1,
                    _ => end = edge - 1,
                }
            }
        } else if node != NO_EDGE && (node as usize) < self.edges.len() && self.occupied(node) {
            let mut e = node;
            loop {
                let rec = self.rec(e);
                if self.letter(rec) == unichar_id && (!word_end || self.eow(rec)) {
                    return e;
                }
                let last = self.last_edge(e);
                e += 1;
                if last || e as usize >= self.edges.len() {
                    break;
                }
            }
        }
        NO_EDGE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DawgPosition {
    pub(crate) dawg_ref: i64,
    pub(crate) punc_ref: i64,
    pub(crate) dawg_index: i8,
    pub(crate) punc_index: i8,
    pub(crate) back_to_punc: bool,
}

impl DawgPosition {
    fn new(dawg_index: i32, dawg_ref: i64, punc_index: i32, punc_ref: i64, back: bool) -> Self {
        DawgPosition {
            dawg_ref,
            punc_ref,
            dawg_index: dawg_index as i8,
            punc_index: punc_index as i8,
            back_to_punc: back,
        }
    }
}

fn add_unique(v: &mut Vec<DawgPosition>, p: DawgPosition) {
    if !v.contains(&p) {
        v.push(p);
    }
}

pub(crate) struct Dict {
    pub(crate) dawgs: Vec<Dawg>,
    successors: Vec<Vec<usize>>,
    punc: Option<usize>,
}

pub(crate) struct DawgArgs {
    pub(crate) updated: Vec<DawgPosition>,
    pub(crate) permuter: i32,
    pub(crate) valid_end: bool,
}

impl Dict {
    pub(crate) fn new(dawgs: Vec<Dawg>) -> Option<Dict> {
        if dawgs.is_empty() {
            return None;
        }
        let successors = dawgs
            .iter()
            .map(|d| {
                (0..dawgs.len())
                    .filter(|&j| successor(d.kind, dawgs[j].kind))
                    .collect()
            })
            .collect();
        let punc = dawgs.iter().position(|d| d.kind == DawgType::Punctuation);
        Some(Dict {
            dawgs,
            successors,
            punc,
        })
    }

    fn starting_node(&self, dawg: &Dawg, edge_ref: i64) -> i64 {
        if edge_ref == NO_EDGE {
            return 0;
        }
        let node = dawg.next_node(edge_ref);
        if node == 0 { NO_EDGE } else { node }
    }

    fn char_for_dawg(&self, set: &Unicharset, ch: i32, dawg: &Dawg) -> i32 {
        if dawg.kind == DawgType::Number && set.is_digit(ch) {
            PATTERN_UNICHAR_ID
        } else {
            ch
        }
    }

    /// `Dict::valid_word(word, false)` (no hyphenated prefix).
    pub(crate) fn valid_word(&self, set: &Unicharset, ids: &[i32]) -> i32 {
        if ids.is_empty() {
            return NO_PERM;
        }
        let mut active = self.default_dawgs();
        let mut args = DawgArgs {
            updated: Vec::new(),
            permuter: NO_PERM,
            valid_end: false,
        };
        let last = ids.len() - 1;
        for (i, &id) in ids.iter().enumerate() {
            if self.letter_is_okay(&active, &mut args, set, id, i == last) == NO_PERM {
                break;
            }
            active = std::mem::take(&mut args.updated);
        }
        if valid_word_permuter(args.permuter) {
            args.permuter
        } else {
            NO_PERM
        }
    }

    /// `Dict::default_dawgs(vec, false)`.
    pub(crate) fn default_dawgs(&self) -> Vec<DawgPosition> {
        let punc_available = self
            .punc
            .is_some_and(|p| self.dawgs[p].edge_char_of(0, PATTERN_UNICHAR_ID, true) != NO_EDGE);
        let mut v = Vec::new();
        for (i, d) in self.dawgs.iter().enumerate() {
            let subsumed = successor(DawgType::Punctuation, d.kind);
            if d.kind == DawgType::Punctuation {
                v.push(DawgPosition::new(-1, NO_EDGE, i as i32, NO_EDGE, false));
            } else if !punc_available || !subsumed {
                v.push(DawgPosition::new(i as i32, NO_EDGE, -1, NO_EDGE, false));
            }
        }
        v
    }

    /// `Dict::def_letter_is_okay` (no pattern dawgs in LSTM models).
    pub(crate) fn letter_is_okay(
        &self,
        active: &[DawgPosition],
        args: &mut DawgArgs,
        set: &Unicharset,
        unichar_id: i32,
        word_end: bool,
    ) -> i32 {
        if unichar_id == PATTERN_UNICHAR_ID || unichar_id == INVALID_UNICHAR_ID {
            args.permuter = NO_PERM;
            return NO_PERM;
        }
        let mut curr_perm = NO_PERM;
        args.updated.clear();
        args.valid_end = false;
        for pos in active {
            let punc = (pos.punc_index >= 0).then(|| &self.dawgs[pos.punc_index as usize]);
            let dawg = (pos.dawg_index >= 0).then(|| &self.dawgs[pos.dawg_index as usize]);
            let Some(dawg) = dawg else {
                let Some(punc) = punc else { continue };
                let punc_node = self.starting_node(punc, pos.punc_ref);
                let trans = punc.edge_char_of(punc_node, PATTERN_UNICHAR_ID, word_end);
                if trans != NO_EDGE {
                    for &si in &self.successors[pos.punc_index as usize] {
                        let sd = &self.dawgs[si];
                        let ch = self.char_for_dawg(set, unichar_id, sd);
                        let e = sd.edge_char_of(0, ch, word_end);
                        if e != NO_EDGE {
                            add_unique(
                                &mut args.updated,
                                DawgPosition::new(
                                    si as i32,
                                    e,
                                    i32::from(pos.punc_index),
                                    trans,
                                    false,
                                ),
                            );
                            if sd.perm > curr_perm {
                                curr_perm = sd.perm;
                            }
                            if sd.end_of_word(e) && punc.end_of_word(trans) {
                                args.valid_end = true;
                            }
                        }
                    }
                }
                let pe = punc.edge_char_of(punc_node, unichar_id, word_end);
                if pe != NO_EDGE {
                    add_unique(
                        &mut args.updated,
                        DawgPosition::new(-1, NO_EDGE, i32::from(pos.punc_index), pe, false),
                    );
                    if PUNC_PERM > curr_perm {
                        curr_perm = PUNC_PERM;
                    }
                    if punc.end_of_word(pe) {
                        args.valid_end = true;
                    }
                }
                continue;
            };
            if let Some(punc) = punc
                && dawg.end_of_word(pos.dawg_ref)
            {
                let punc_node = self.starting_node(punc, pos.punc_ref);
                let pe = if punc_node == NO_EDGE {
                    NO_EDGE
                } else {
                    punc.edge_char_of(punc_node, unichar_id, word_end)
                };
                if pe != NO_EDGE {
                    add_unique(
                        &mut args.updated,
                        DawgPosition::new(
                            i32::from(pos.dawg_index),
                            pos.dawg_ref,
                            i32::from(pos.punc_index),
                            pe,
                            true,
                        ),
                    );
                    if dawg.perm > curr_perm {
                        curr_perm = dawg.perm;
                    }
                    if punc.end_of_word(pe) {
                        args.valid_end = true;
                    }
                }
            }
            if pos.back_to_punc {
                continue;
            }
            let node = self.starting_node(dawg, pos.dawg_ref);
            let edge = if node == NO_EDGE {
                NO_EDGE
            } else {
                dawg.edge_char_of(node, self.char_for_dawg(set, unichar_id, dawg), word_end)
            };
            if edge != NO_EDGE {
                if word_end && punc.is_some_and(|p| !p.end_of_word(pos.punc_ref)) {
                    continue;
                }
                if dawg.perm > curr_perm {
                    curr_perm = dawg.perm;
                }
                if dawg.end_of_word(edge) && punc.is_none_or(|p| p.end_of_word(pos.punc_ref)) {
                    args.valid_end = true;
                }
                add_unique(
                    &mut args.updated,
                    DawgPosition::new(
                        i32::from(pos.dawg_index),
                        edge,
                        i32::from(pos.punc_index),
                        pos.punc_ref,
                        false,
                    ),
                );
            }
        }
        if args.permuter == NO_PERM
            || curr_perm == NO_PERM
            || (curr_perm != PUNC_PERM && args.permuter != COMPOUND_PERM)
        {
            args.permuter = curr_perm;
        }
        args.permuter
    }
}

/// `Dict::valid_word_permuter(perm, false)`.
pub(crate) fn valid_word_permuter(perm: i32) -> bool {
    // SYSTEM, DOC, USER, FREQ dawgs, user patterns and compounds.
    matches!(perm, 7..=12)
}
