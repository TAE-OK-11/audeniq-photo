//! Port of `RecodeBeamSearch` (CTC beam search over recoded labels with
//! dictionary beams), including Tesseract's own heap so ties resolve
//! identically.

use crate::dict::{DawgArgs, DawgPosition, Dict, NO_PERM, TOP_CHOICE_PERM};
use crate::unicharset::{INVALID_UNICHAR_ID, Recoder, UNICHAR_SPACE, Unicharset};

const BEAM_WIDTHS: [usize; 10] = [5, 10, 16, 16, 16, 16, 16, 16, 16, 16];
const NUM_LENGTHS: usize = 10;
const NC_ANYTHING: usize = 0;
const NC_ONLY_DUP: usize = 1;
const NC_NO_DUP: usize = 2;
const NC_COUNT: usize = 3;
const NUM_BEAMS: usize = 2 * NC_COUNT * NUM_LENGTHS;
const MIN_CERTAINTY: f32 = -20.0;

const TN_TOP2: u8 = 0;
const TN_TOPN: u8 = 1;
const TN_ALSO_RAN: u8 = 2;

/// `NetworkIO::ProbToCertainty`.
pub(crate) fn prob_to_certainty(prob: f32) -> f32 {
    let min_prob = MIN_CERTAINTY.exp();
    if prob > min_prob {
        prob.ln()
    } else {
        MIN_CERTAINTY
    }
}

fn beam_index(is_dawg: bool, cont: usize, length: usize) -> usize {
    (usize::from(is_dawg) * NC_COUNT + cont) * NUM_LENGTHS + length
}

/// Location of a node in a finished beam step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodeRef {
    t: u32,
    beam: u16,
    slot: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) code: i32,
    pub(crate) unichar_id: i32,
    pub(crate) permuter: i32,
    pub(crate) start_of_dawg: bool,
    pub(crate) start_of_word: bool,
    pub(crate) end_of_word: bool,
    pub(crate) duplicate: bool,
    pub(crate) certainty: f32,
    pub(crate) score: f32,
    prev: Option<NodeRef>,
    dawgs: Option<Vec<DawgPosition>>,
    code_hash: u64,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            code: -1,
            unichar_id: INVALID_UNICHAR_ID,
            permuter: TOP_CHOICE_PERM,
            start_of_dawg: false,
            start_of_word: false,
            end_of_word: false,
            duplicate: false,
            certainty: 0.0,
            score: 0.0,
            prev: None,
            dawgs: None,
            code_hash: 0,
        }
    }
}

/// Tesseract `GenericHeap<KDPairInc<K, D>>`: a min-heap on the key.
#[derive(Clone, Debug, Default)]
struct Heap<K: Copy + PartialOrd, D: Clone> {
    v: Vec<(K, D)>,
}

impl<K: Copy + PartialOrd, D: Clone> Heap<K, D> {
    fn clear(&mut self) {
        self.v.clear();
    }

    fn len(&self) -> usize {
        self.v.len()
    }

    fn peek_top(&self) -> &(K, D) {
        &self.v[0]
    }

    fn push(&mut self, entry: (K, D)) {
        let hole = self.v.len();
        self.v.push(entry.clone());
        let hole = self.sift_up(hole, entry.0);
        self.v[hole] = entry;
    }

    fn pop(&mut self) -> Option<(K, D)> {
        let new_size = self.v.len().checked_sub(1)?;
        let top = self.v[0].clone();
        if new_size > 0 {
            let hole_pair = self.v[new_size].clone();
            self.v.truncate(new_size);
            let hole = self.sift_down(0, hole_pair.0);
            self.v[hole] = hole_pair;
        } else {
            self.v.clear();
        }
        Some(top)
    }

    fn reshuffle(&mut self, index: usize) {
        let hole_pair = self.v[index].clone();
        let i = self.sift_down(index, hole_pair.0);
        let i = self.sift_up(i, hole_pair.0);
        self.v[i] = hole_pair;
    }

    fn sift_up(&mut self, mut hole: usize, key: K) -> usize {
        while hole > 0 {
            let parent = hole.div_ceil(2) - 1;
            if key < self.v[parent].0 {
                self.v[hole] = self.v[parent].clone();
                hole = parent;
            } else {
                break;
            }
        }
        hole
    }

    fn sift_down(&mut self, mut hole: usize, key: K) -> usize {
        let size = self.v.len();
        loop {
            let mut child = hole * 2 + 1;
            if child >= size {
                break;
            }
            if child + 1 < size && self.v[child + 1].0 < self.v[child].0 {
                child += 1;
            }
            if self.v[child].0 < key {
                self.v[hole] = self.v[child].clone();
                hole = child;
            } else {
                break;
            }
        }
        hole
    }
}

#[derive(Clone, Debug, Default)]
struct Step {
    beams: Vec<Heap<f64, Node>>,
    best_initial: [Node; NC_COUNT],
}

impl Step {
    fn new() -> Self {
        Step {
            beams: vec![Heap::default(); NUM_BEAMS],
            best_initial: Default::default(),
        }
    }

    fn clear(&mut self) {
        self.beams.iter_mut().for_each(Heap::clear);
        self.best_initial = Default::default();
    }
}

pub(crate) struct Search<'a> {
    recoder: &'a Recoder,
    dict: Option<&'a Dict>,
    set: &'a Unicharset,
    null_char: i32,
    space_delimited: bool,
    beam: Vec<Step>,
    top_flags: Vec<u8>,
    top_code: i32,
    second_code: i32,
}

/// One recognized word (`WERD_RES` reduced to what the backend reads).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    /// `best_choice->certainty()` after `SearchWords` (× 7).
    pub certainty: f32,
    /// Tesseract word confidence: `clip(100 + 5 * certainty, 0, 100)`.
    pub confidence: f32,
    /// Start/end x in network time steps (scaled later).
    pub(crate) start_t: i32,
    pub(crate) end_t: i32,
    /// Unichar ids of the word's characters.
    pub(crate) ids: Vec<i32>,
    /// `WERD_CHOICE::rating()`.
    pub(crate) rating: f32,
    /// Permuter of the last character's node.
    pub(crate) permuter: i32,
    /// Character boundaries (time steps): character `i` spans
    /// `bounds[i]..bounds[i + 1]`.
    pub(crate) bounds: Vec<i32>,
    /// True when every character is a space (deleted after recognition).
    pub(crate) all_spaces: bool,
}

impl<'a> Search<'a> {
    pub(crate) fn new(
        recoder: &'a Recoder,
        null_char: i32,
        dict: Option<&'a Dict>,
        set: &'a Unicharset,
    ) -> Self {
        let space_delimited = dict.is_none_or(|_| set.space_delimited_lang());
        Search {
            recoder,
            dict,
            set,
            null_char,
            space_delimited,
            beam: Vec::new(),
            top_flags: Vec::new(),
            top_code: -1,
            second_code: -1,
        }
    }

    fn node(&self, r: NodeRef) -> &Node {
        &self.beam[r.t as usize].beams[r.beam as usize].v[r.slot as usize].1
    }

    /// `Decode` over float softmax rows.
    pub(crate) fn decode(
        &mut self,
        rows: &[&[f32]],
        dict_ratio: f64,
        cert_offset: f64,
        worst_dict_cert: f64,
    ) {
        for (t, row) in rows.iter().enumerate() {
            self.compute_top_n(row, BEAM_WIDTHS[0]);
            self.decode_step(row, t, dict_ratio, cert_offset, worst_dict_cert);
        }
        self.beam.truncate(rows.len());
    }

    fn compute_top_n(&mut self, outputs: &[f32], top_n: usize) {
        self.top_flags.clear();
        self.top_flags.resize(outputs.len(), TN_ALSO_RAN);
        self.top_code = -1;
        self.second_code = -1;
        let mut heap: Heap<f32, i32> = Heap::default();
        for (i, &o) in outputs.iter().enumerate() {
            if heap.len() < top_n || o > heap.peek_top().0 {
                heap.push((o, i as i32));
                if heap.len() > top_n {
                    heap.pop();
                }
            }
        }
        while let Some((_, d)) = heap.pop() {
            if heap.len() > 1 {
                self.top_flags[d as usize] = TN_TOPN;
            } else {
                self.top_flags[d as usize] = TN_TOP2;
                if heap.len() == 0 {
                    self.top_code = d;
                } else {
                    self.second_code = d;
                }
            }
        }
        self.top_flags[self.null_char as usize] = TN_TOP2;
    }

    fn decode_step(
        &mut self,
        outputs: &[f32],
        t: usize,
        dict_ratio: f64,
        cert_offset: f64,
        worst: f64,
    ) {
        if t == self.beam.len() {
            self.beam.push(Step::new());
        }
        let mut step = std::mem::take(&mut self.beam[t]);
        if step.beams.is_empty() {
            step = Step::new();
        }
        step.clear();
        if t == 0 {
            self.continue_context(
                None,
                beam_index(false, NC_ANYTHING, 0),
                outputs,
                TN_TOP2,
                dict_ratio,
                cert_offset,
                worst,
                &mut step,
            );
            if self.dict.is_some() {
                self.continue_context(
                    None,
                    beam_index(true, NC_ANYTHING, 0),
                    outputs,
                    TN_TOP2,
                    dict_ratio,
                    cert_offset,
                    worst,
                    &mut step,
                );
            }
        } else {
            let mut total = 0;
            for tn in [TN_TOP2, TN_TOPN, TN_ALSO_RAN] {
                if total != 0 {
                    break;
                }
                for index in 0..NUM_BEAMS {
                    let n = self.beam[t - 1].beams[index].len();
                    for i in (0..n).rev() {
                        let r = NodeRef {
                            t: (t - 1) as u32,
                            beam: index as u16,
                            slot: i as u16,
                        };
                        self.continue_context(
                            Some(r),
                            index,
                            outputs,
                            tn,
                            dict_ratio,
                            cert_offset,
                            worst,
                            &mut step,
                        );
                    }
                }
                for index in 0..NUM_BEAMS {
                    if (index / NUM_LENGTHS) % NC_COUNT == NC_ANYTHING {
                        total += step.beams[index].len();
                    }
                }
            }
            for c in 0..NC_COUNT {
                if step.best_initial[c].code >= 0 {
                    let node = std::mem::take(&mut step.best_initial[c]);
                    let idx = beam_index(true, c, 0);
                    push_node_if_better(BEAM_WIDTHS[0], node, &mut step.beams[idx]);
                }
            }
        }
        self.beam[t] = step;
    }

    #[allow(clippy::too_many_arguments)]
    fn continue_context(
        &self,
        prev: Option<NodeRef>,
        index: usize,
        outputs: &[f32],
        top_n_flag: u8,
        dict_ratio: f64,
        cert_offset: f64,
        worst: f64,
        step: &mut Step,
    ) {
        let length = index % NUM_LENGTHS;
        let use_dawgs = index / (NUM_LENGTHS * NC_COUNT) > 0;
        let prev_cont = (index / NUM_LENGTHS) % NC_COUNT;
        let mut prefix = [0i32; 9];
        let mut prefix_len = 0usize;
        let mut previous = prev;
        for p in (0..length).rev() {
            while let Some(r) = previous {
                let n = self.node(r);
                if n.duplicate || n.code == self.null_char {
                    previous = n.prev;
                } else {
                    break;
                }
            }
            if let Some(r) = previous {
                prefix[p] = self.node(r).code;
                prefix_len = prefix_len.max(p + 1);
                previous = self.node(r).prev;
            } else {
                break;
            }
        }
        let mut full = prefix;
        let worst_f = worst as f32;
        let ratio_f = dict_ratio as f32;
        let cert_of = |p: f32| (f64::from(prob_to_certainty(p)) + cert_offset) as f32;
        let prev_node = prev.map(|r| self.node(r));
        if let Some(pn) = prev_node {
            if self.top_flags[pn.code as usize] == top_n_flag {
                if prev_cont != NC_NO_DUP {
                    let cert = cert_of(outputs[pn.code as usize]);
                    self.push_dup_or_no_dawg(
                        length,
                        true,
                        pn.code,
                        pn.unichar_id,
                        cert,
                        worst_f,
                        ratio_f,
                        use_dawgs,
                        NC_ANYTHING,
                        prev,
                        step,
                    );
                }
                if prev_cont == NC_ANYTHING && top_n_flag == TN_TOP2 && pn.code != self.null_char {
                    let cert =
                        cert_of(outputs[pn.code as usize] + outputs[self.null_char as usize]);
                    self.push_dup_or_no_dawg(
                        length,
                        true,
                        pn.code,
                        pn.unichar_id,
                        cert,
                        worst_f,
                        ratio_f,
                        use_dawgs,
                        NC_NO_DUP,
                        prev,
                        step,
                    );
                }
            }
            if prev_cont == NC_ONLY_DUP {
                return;
            }
            if pn.code != self.null_char
                && length > 0
                && self.top_flags[self.null_char as usize] == top_n_flag
            {
                let cert = cert_of(outputs[self.null_char as usize]);
                self.push_dup_or_no_dawg(
                    length,
                    false,
                    self.null_char,
                    INVALID_UNICHAR_ID,
                    cert,
                    worst_f,
                    ratio_f,
                    use_dawgs,
                    NC_ANYTHING,
                    prev,
                    step,
                );
            }
        }
        let pair_bonus = |code: i32| -> bool {
            prev_node.is_some_and(|pn| {
                prev_cont == NC_ANYTHING
                    && pn.code != self.null_char
                    && ((pn.code == self.top_code && code == self.second_code)
                        || (code == self.top_code && pn.code == self.second_code))
            })
        };
        if let Some(final_codes) = self.recoder.final_codes(&prefix[..prefix_len]) {
            for &code in final_codes {
                if self.top_flags[code as usize] != top_n_flag {
                    continue;
                }
                if prev_node.is_some_and(|pn| pn.code == code) {
                    continue;
                }
                let mut cert = cert_of(outputs[code as usize]);
                if cert < MIN_CERTAINTY && code != self.null_char {
                    continue;
                }
                full[length] = code;
                let mut unichar_id = self.recoder.decode(&full[..length + 1]);
                if length == 0 && code == self.null_char {
                    unichar_id = INVALID_UNICHAR_ID;
                }
                self.continue_unichar(
                    code,
                    unichar_id,
                    cert,
                    worst_f,
                    ratio_f,
                    use_dawgs,
                    NC_ANYTHING,
                    prev,
                    step,
                );
                if top_n_flag == TN_TOP2 && code != self.null_char {
                    let mut prob = outputs[code as usize] + outputs[self.null_char as usize];
                    if pair_bonus(code) {
                        prob += outputs[prev_node.expect("bonus needs prev").code as usize];
                    }
                    cert = cert_of(prob);
                    self.continue_unichar(
                        code,
                        unichar_id,
                        cert,
                        worst_f,
                        ratio_f,
                        use_dawgs,
                        NC_ONLY_DUP,
                        prev,
                        step,
                    );
                }
            }
        }
        if let Some(next_codes) = self.recoder.next_codes(&prefix[..prefix_len]) {
            for &code in next_codes {
                if self.top_flags[code as usize] != top_n_flag {
                    continue;
                }
                if prev_node.is_some_and(|pn| pn.code == code) {
                    continue;
                }
                let cert = cert_of(outputs[code as usize]);
                self.push_dup_or_no_dawg(
                    length + 1,
                    false,
                    code,
                    INVALID_UNICHAR_ID,
                    cert,
                    worst_f,
                    ratio_f,
                    use_dawgs,
                    NC_ANYTHING,
                    prev,
                    step,
                );
                if top_n_flag == TN_TOP2 && code != self.null_char {
                    let mut prob = outputs[code as usize] + outputs[self.null_char as usize];
                    if pair_bonus(code) {
                        prob += outputs[prev_node.expect("bonus needs prev").code as usize];
                    }
                    let cert = cert_of(prob);
                    self.push_dup_or_no_dawg(
                        length + 1,
                        false,
                        code,
                        INVALID_UNICHAR_ID,
                        cert,
                        worst_f,
                        ratio_f,
                        use_dawgs,
                        NC_ONLY_DUP,
                        prev,
                        step,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn continue_unichar(
        &self,
        code: i32,
        unichar_id: i32,
        cert: f32,
        worst: f32,
        dict_ratio: f32,
        use_dawgs: bool,
        cont: usize,
        prev: Option<NodeRef>,
        step: &mut Step,
    ) {
        if use_dawgs {
            if cert > worst {
                self.continue_dawg(code, unichar_id, cert, cont, prev, step);
            }
        } else {
            let idx = beam_index(false, cont, 0);
            self.push_heap_if_better(
                BEAM_WIDTHS[0],
                code,
                unichar_id,
                TOP_CHOICE_PERM,
                false,
                false,
                false,
                false,
                cert * dict_ratio,
                prev,
                None,
                &mut step.beams[idx],
            );
            if self.dict.is_some()
                && ((unichar_id == UNICHAR_SPACE && cert > worst)
                    || !self.set.is_space_delimited(unichar_id))
            {
                let mut dawg_cert = cert;
                let mut permuter = TOP_CHOICE_PERM;
                if unichar_id == UNICHAR_SPACE {
                    permuter = NO_PERM;
                } else {
                    dawg_cert *= dict_ratio;
                }
                self.push_initial_dawg_if_better(
                    code, unichar_id, permuter, false, false, dawg_cert, cont, prev, step,
                );
            }
        }
    }

    fn continue_dawg(
        &self,
        code: i32,
        unichar_id: i32,
        cert: f32,
        cont: usize,
        prev: Option<NodeRef>,
        step: &mut Step,
    ) {
        let dict = self.dict.expect("dawg beam without dict");
        let dawg_idx = beam_index(true, cont, 0);
        let nodawg_idx = beam_index(false, cont, 0);
        if unichar_id == INVALID_UNICHAR_ID {
            self.push_heap_if_better(
                BEAM_WIDTHS[0],
                code,
                unichar_id,
                NO_PERM,
                false,
                false,
                false,
                false,
                cert,
                prev,
                None,
                &mut step.beams[dawg_idx],
            );
            return;
        }
        let mut score = cert;
        if let Some(p) = prev {
            score += self.node(p).score;
        }
        let w = BEAM_WIDTHS[0];
        if step.beams[dawg_idx].len() >= w
            && score <= step.beams[dawg_idx].peek_top().1.score
            && step.beams[nodawg_idx].len() >= w
            && score <= step.beams[nodawg_idx].peek_top().1.score
        {
            return;
        }
        let mut uni_prev = prev;
        while let Some(r) = uni_prev {
            let n = self.node(r);
            if n.unichar_id == INVALID_UNICHAR_ID || n.duplicate {
                uni_prev = n.prev;
            } else {
                break;
            }
        }
        let up = uni_prev.map(|r| self.node(r));
        if unichar_id == UNICHAR_SPACE {
            if let Some(u) = up
                && u.end_of_word
            {
                self.push_initial_dawg_if_better(
                    code, unichar_id, u.permuter, false, false, cert, cont, prev, step,
                );
                self.push_heap_if_better(
                    w,
                    code,
                    unichar_id,
                    u.permuter,
                    false,
                    false,
                    false,
                    false,
                    cert,
                    prev,
                    None,
                    &mut step.beams[nodawg_idx],
                );
            }
            return;
        } else if let Some(u) = up
            && u.start_of_dawg
            && u.unichar_id != UNICHAR_SPACE
            && self.set.is_space_delimited(u.unichar_id)
            && self.set.is_space_delimited(unichar_id)
        {
            return;
        }
        let initial;
        let (active, word_start): (&[DawgPosition], bool) = match up {
            None => {
                initial = dict.default_dawgs();
                (&initial, true)
            }
            Some(u) => match &u.dawgs {
                Some(d) => (d.as_slice(), u.start_of_dawg),
                None => return,
            },
        };
        let mut args = DawgArgs {
            updated: Vec::new(),
            permuter: NO_PERM,
            valid_end: false,
        };
        let permuter = dict.letter_is_okay(active, &mut args, self.set, unichar_id, false);
        if permuter != NO_PERM {
            let valid_end = args.valid_end;
            self.push_heap_if_better(
                w,
                code,
                unichar_id,
                permuter,
                false,
                word_start,
                valid_end,
                false,
                cert,
                prev,
                Some(args.updated),
                &mut step.beams[dawg_idx],
            );
            if valid_end && !self.space_delimited {
                self.push_initial_dawg_if_better(
                    code, unichar_id, permuter, word_start, true, cert, cont, prev, step,
                );
                self.push_heap_if_better(
                    w,
                    code,
                    unichar_id,
                    permuter,
                    false,
                    word_start,
                    true,
                    false,
                    cert,
                    prev,
                    None,
                    &mut step.beams[nodawg_idx],
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_initial_dawg_if_better(
        &self,
        code: i32,
        unichar_id: i32,
        permuter: i32,
        start: bool,
        end: bool,
        cert: f32,
        cont: usize,
        prev: Option<NodeRef>,
        step: &mut Step,
    ) {
        let mut score = cert;
        if let Some(p) = prev {
            score += self.node(p).score;
        }
        let best = &step.best_initial[cont];
        if best.code < 0 || score > best.score {
            let dawgs = self.dict.map(Dict::default_dawgs);
            step.best_initial[cont] = Node {
                code,
                unichar_id,
                permuter,
                start_of_dawg: true,
                start_of_word: start,
                end_of_word: end,
                duplicate: false,
                certainty: cert,
                score,
                prev,
                dawgs,
                code_hash: self.code_hash(code, false, prev),
            };
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_dup_or_no_dawg(
        &self,
        length: usize,
        dup: bool,
        code: i32,
        unichar_id: i32,
        cert: f32,
        worst: f32,
        dict_ratio: f32,
        use_dawgs: bool,
        cont: usize,
        prev: Option<NodeRef>,
        step: &mut Step,
    ) {
        let idx = beam_index(use_dawgs, cont, length);
        let prev_perm = prev.map(|p| self.node(p).permuter);
        if use_dawgs {
            if cert > worst {
                self.push_heap_if_better(
                    BEAM_WIDTHS[length],
                    code,
                    unichar_id,
                    prev_perm.unwrap_or(NO_PERM),
                    false,
                    false,
                    false,
                    dup,
                    cert,
                    prev,
                    None,
                    &mut step.beams[idx],
                );
            }
        } else {
            let cert = cert * dict_ratio;
            if cert >= MIN_CERTAINTY || code == self.null_char {
                self.push_heap_if_better(
                    BEAM_WIDTHS[length],
                    code,
                    unichar_id,
                    prev_perm.unwrap_or(TOP_CHOICE_PERM),
                    false,
                    false,
                    false,
                    dup,
                    cert,
                    prev,
                    None,
                    &mut step.beams[idx],
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_heap_if_better(
        &self,
        max_size: usize,
        code: i32,
        unichar_id: i32,
        permuter: i32,
        dawg_start: bool,
        word_start: bool,
        end: bool,
        dup: bool,
        cert: f32,
        prev: Option<NodeRef>,
        dawgs: Option<Vec<DawgPosition>>,
        heap: &mut Heap<f64, Node>,
    ) {
        let mut score = cert;
        if let Some(p) = prev {
            score += self.node(p).score;
        }
        if heap.len() < max_size || score > heap.peek_top().1.score {
            let node = Node {
                code,
                unichar_id,
                permuter,
                start_of_dawg: dawg_start,
                start_of_word: word_start,
                end_of_word: end,
                duplicate: dup,
                certainty: cert,
                score,
                prev,
                dawgs,
                code_hash: self.code_hash(code, dup, prev),
            };
            if update_heap_if_matched(&node, heap) {
                return;
            }
            heap.push((f64::from(score), node));
            if heap.len() > max_size {
                heap.pop();
            }
        }
    }

    fn code_hash(&self, code: i32, dup: bool, prev: Option<NodeRef>) -> u64 {
        let mut hash = prev.map_or(0, |p| self.node(p).code_hash);
        if !dup && code != self.null_char {
            let n = self.recoder.code_range as u64;
            let carry = ((hash >> 32).wrapping_mul(n)) >> 32;
            hash = hash.wrapping_mul(n);
            hash = hash.wrapping_add(carry);
            hash = hash.wrapping_add(code as u64);
        }
        hash
    }

    /// `ExtractBestPaths` (best path only).
    fn best_path(&self) -> Vec<NodeRef> {
        let Some(last_t) = self.beam.len().checked_sub(1) else {
            return Vec::new();
        };
        let last = &self.beam[last_t];
        let mut best: Option<NodeRef> = None;
        for c in [NC_ANYTHING, NC_NO_DUP] {
            for is_dawg in [false, true] {
                let bi = beam_index(is_dawg, c, 0);
                for h in 0..last.beams[bi].len() {
                    let r = NodeRef {
                        t: last_t as u32,
                        beam: bi as u16,
                        slot: h as u16,
                    };
                    let node = self.node(r);
                    if is_dawg {
                        let mut dn = Some(r);
                        while let Some(d) = dn {
                            let n = self.node(d);
                            if n.unichar_id == INVALID_UNICHAR_ID || n.duplicate {
                                dn = n.prev;
                            } else {
                                break;
                            }
                        }
                        match dn.map(|d| self.node(d)) {
                            None => continue,
                            Some(n) if !n.end_of_word && n.unichar_id != UNICHAR_SPACE => continue,
                            _ => {}
                        }
                    }
                    if best.is_none_or(|b| node.score > self.node(b).score) {
                        best = Some(r);
                    }
                }
            }
        }
        let mut path = Vec::new();
        let mut n = best;
        while let Some(r) = n {
            path.push(r);
            n = self.node(r).prev;
        }
        path.reverse();
        path
    }

    /// `ExtractBestPathAsWords` + `SearchWords` certainty scaling.
    pub(crate) fn words(&self) -> Vec<Word> {
        let path = self.best_path();
        let nodes: Vec<&Node> = path.iter().map(|&r| self.node(r)).collect();
        let (ids, certs, ratings, xcoords, bounds) = extract_unichar_ids(&nodes);
        let num = ids.len();
        let mut words = Vec::new();
        let mut word_end = 0;
        let mut prev_space_cert = 0.0f32;
        let mut word_start = 0;
        while word_start < num {
            word_end = word_start + 1;
            while word_end < num {
                if ids[word_end] == UNICHAR_SPACE {
                    break;
                }
                let index = xcoords[word_end] as usize;
                if nodes[index].start_of_word {
                    break;
                }
                if nodes[index].permuter == TOP_CHOICE_PERM
                    && (!self.set.is_space_delimited(ids[word_end])
                        || !self.set.is_space_delimited(ids[word_end - 1]))
                {
                    break;
                }
                word_end += 1;
            }
            let mut space_cert = 0.0f32;
            if word_end < num && ids[word_end] == UNICHAR_SPACE {
                space_cert = certs[word_end];
            }
            let space_certainty = space_cert.min(prev_space_cert);
            let mut text = String::new();
            let mut cert = f32::MAX;
            let mut rating = 0.0f32;
            for i in word_start..word_end {
                text.push_str(self.set.text(ids[i]));
                if certs[i] < cert {
                    cert = certs[i];
                }
                rating += ratings[i];
            }
            let certainty = space_certainty.min(cert) * 7.0;
            let confidence = (100.0 + 5.0 * certainty).clamp(0.0, 100.0);
            let start_t = bounds.get(word_start).copied().unwrap_or(0);
            let end_t = bounds.get(word_end).copied().unwrap_or(nodes.len() as i32);
            let word_bounds: Vec<i32> = (word_start..=word_end)
                .take_while(|&i| i < bounds.len())
                .map(|i| bounds[i])
                .collect();
            words.push(Word {
                text,
                certainty,
                confidence,
                start_t,
                end_t,
                ids: ids[word_start..word_end].to_vec(),
                rating,
                permuter: nodes[xcoords[word_end - 1] as usize].permuter,
                bounds: word_bounds,
                all_spaces: ids[word_start..word_end]
                    .iter()
                    .all(|&u| u == UNICHAR_SPACE),
            });
            prev_space_cert = space_cert;
            if word_end < num && ids[word_end] == UNICHAR_SPACE {
                word_end += 1;
            }
            word_start = word_end;
        }
        let _ = word_end;
        words
    }
}

fn update_heap_if_matched(new: &Node, heap: &mut Heap<f64, Node>) -> bool {
    for i in 0..heap.v.len() {
        let node = &heap.v[i].1;
        if node.code == new.code
            && node.code_hash == new.code_hash
            && node.permuter == new.permuter
            && node.start_of_dawg == new.start_of_dawg
        {
            if new.score > node.score {
                heap.v[i] = (f64::from(new.score), new.clone());
                heap.reshuffle(i);
            }
            return true;
        }
    }
    false
}

fn push_node_if_better(max_size: usize, node: Node, heap: &mut Heap<f64, Node>) {
    if heap.len() < max_size || node.score > heap.peek_top().1.score {
        if update_heap_if_matched(&node, heap) {
            return;
        }
        heap.push((f64::from(node.score), node));
        if heap.len() > max_size {
            heap.pop();
        }
    }
}

/// `ExtractPathAsUnicharIds` + `calculateCharBoundaries`.
#[allow(clippy::type_complexity)]
fn extract_unichar_ids(nodes: &[&Node]) -> (Vec<i32>, Vec<f32>, Vec<f32>, Vec<i32>, Vec<i32>) {
    let (mut ids, mut certs, mut xcoords) = (Vec::new(), Vec::<f32>::new(), Vec::new());
    let mut ratings = Vec::<f32>::new();
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
    let width = nodes.len();
    let mut t = 0;
    while t < width {
        let mut certainty = 0.0f64;
        let mut rating = 0.0f64;
        while t < width && nodes[t].unichar_id == INVALID_UNICHAR_ID {
            let c = f64::from(nodes[t].certainty);
            t += 1;
            if c < certainty {
                certainty = c;
            }
            rating -= c;
        }
        starts.push(t as i32);
        if t < width {
            let uid = nodes[t].unichar_id;
            if uid == UNICHAR_SPACE && !certs.is_empty() && nodes[t].permuter != NO_PERM {
                if certainty < f64::from(*certs.last().expect("non-empty")) {
                    *certs.last_mut().expect("non-empty") = certainty as f32;
                }
                let r = ratings.last_mut().expect("non-empty");
                *r = (f64::from(*r) + rating) as f32;
                certainty = 0.0;
                rating = 0.0;
            }
            ids.push(uid);
            xcoords.push(t as i32);
            loop {
                let c = f64::from(nodes[t].certainty);
                t += 1;
                if c < certainty || (uid == UNICHAR_SPACE && nodes[t - 1].permuter == NO_PERM) {
                    certainty = c;
                }
                rating -= c;
                if !(t < width && nodes[t].duplicate) {
                    break;
                }
            }
            ends.push(t as i32);
            certs.push(certainty as f32);
            ratings.push(rating as f32);
        } else if !certs.is_empty() {
            if certainty < f64::from(*certs.last().expect("non-empty")) {
                *certs.last_mut().expect("non-empty") = certainty as f32;
            }
            let r = ratings.last_mut().expect("non-empty");
            *r = (f64::from(*r) + rating) as f32;
        }
    }
    starts.push(width as i32);
    let mut bounds = vec![0];
    for i in 0..ends.len() {
        let middle = (starts[i + 1] - ends[i]) / 2;
        bounds.push(ends[i] + middle);
    }
    bounds.pop();
    bounds.push(width as i32);
    xcoords.push(width as i32);
    (ids, certs, ratings, xcoords, bounds)
}
