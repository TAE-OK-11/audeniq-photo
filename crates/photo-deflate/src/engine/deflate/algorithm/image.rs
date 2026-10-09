#![forbid(unsafe_code)]

//! `Strategy::Image`: greedy matching that keeps only long matches, for
//! PNG-filtered pixel rows. In photographic content the filter residuals
//! are noise-like: a short match (3..7 bytes) costs about as many bits as
//! the literals it replaces under a dynamic Huffman code, yet breaks the
//! literal statistics, so zlib's default levels come out both larger and
//! several times slower than Huffman-only coding. Flat graphics, on the
//! other hand, need matches: there they are long (runs, repeated rows).
//! Accepting only matches of at least `IMAGE_MIN_MATCH` bytes, and only
//! when they are estimated to beat the literals under the block's current
//! statistics, gives the Huffman-only size on photos and LZ77 sizes on
//! graphics. Candidates come from a one-entry table keyed by 8 bytes, so a
//! probe is one load and compare; long runs of misses probe sparser.

use super::flush_block;
use crate::engine::{
    deflate::{
        fill_window, BlockState, DeflateStream, MIN_LOOKAHEAD, STD_MAX_MATCH, STD_MIN_MATCH,
    },
    DeflateFlush,
};

/// Shortest match worth coding in filtered image data.
pub(crate) const IMAGE_MIN_MATCH: usize = 8;

/// Misses before probing gets sparser (2^n consecutive misses per step).
const SKIP_SHIFT: u32 = 7;

/// Table size: 16K entries of `head` (32 KiB, stays in L1).
const HASH_BITS: u32 = 14;

/// Hash of the 8 bytes at a position: candidates are looked up by their
/// first 8 bytes, so nearly every candidate is already a usable match
/// (noise-like data then costs one probe per byte, not a chain walk).
#[inline(always)]
fn hash8(v: u64) -> usize {
    (v.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - HASH_BITS)) as usize
}

#[inline(always)]
fn load8(w: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(w[at..at + 8].try_into().unwrap())
}

/// Length of the common prefix of `w[a..]` and `w[b..]`, at most `max`.
#[inline(always)]
fn common(w: &[u8], a: usize, b: usize, max: usize) -> usize {
    let mut n = 0;
    while n + 8 <= max {
        let x = load8(w, a + n) ^ load8(w, b + n);
        if x != 0 {
            return n + (x.trailing_zeros() / 8) as usize;
        }
        n += 8;
    }
    while n < max && w[a + n] == w[b + n] {
        n += 1;
    }
    n
}

/// Refresh the literal cost table after this many new symbols.
const COST_REFRESH: usize = 4096;

/// Would coding `len` bytes at `start` as a match at `dist` take fewer
/// bits than coding them as literals under the current statistics?
#[inline(always)]
fn worth_it(
    state: &mut crate::engine::deflate::State,
    start: usize,
    len: usize,
    dist: usize,
) -> bool {
    let filled = state.sym_buf.filled();
    if filled < state.image_cost_mark || filled - state.image_cost_mark >= 3 * COST_REFRESH {
        refresh_costs(state);
    }
    let w = state.window.filled();
    let lit: u32 = w[start..start + len]
        .iter()
        .map(|&b| u32::from(state.image_cost[b as usize]))
        .sum();
    // Length symbol ~7 bits plus extra bits; distance symbol ~5 bits plus
    // extra bits (codes 4+ carry floor(log2(dist - 1)) - 1 extra bits).
    let len_extra = match len {
        ..=10 => 0,
        11..=18 => 1,
        19..=34 => 2,
        35..=66 => 3,
        67..=130 => 4,
        131..=257 => 5,
        _ => 0,
    };
    let dist_extra = (usize::BITS - 1 - (dist.max(2) - 1).leading_zeros()).saturating_sub(1);
    let bits = 7 + len_extra + 5 + dist_extra;
    lit > bits * 8
}

/// Literal costs in 1/8 bits, -log2 of the (add-one smoothed) frequency in
/// the block so far; kept as they were until the block has enough data.
fn refresh_costs(state: &mut crate::engine::deflate::State) {
    state.image_cost_mark = state.sym_buf.filled();
    let total: u32 = state.literal_freqs().map(u32::from).sum();
    if total < 1024 {
        return;
    }
    let lt = ((total + 256) as f32).log2();
    let mut costs = [0u8; 256];
    for (c, f) in costs.iter_mut().zip(state.literal_freqs()) {
        *c = ((lt - (f32::from(f) + 1.0).log2()) * 8.0).clamp(1.0, 255.0) as u8;
    }
    state.image_cost = costs;
}

pub fn deflate_image(stream: &mut DeflateStream, flush: DeflateFlush) -> BlockState {
    // Consecutive positions without a match: after a while in noise-like
    // data, probe only every other (then every fourth) position, as LZ4's
    // acceleration does. A long match still starts within a few bytes.
    let mut misses = 0u32;
    loop {
        if stream.state.lookahead < MIN_LOOKAHEAD {
            fill_window(stream);
            if stream.state.lookahead < MIN_LOOKAHEAD && flush == DeflateFlush::NoFlush {
                return BlockState::NeedMore;
            }
            if stream.state.lookahead == 0 {
                break;
            }
        }

        let state = &mut stream.state;
        let start = state.strstart;
        let step = (misses >> SKIP_SHIFT).min(3);
        if state.lookahead >= IMAGE_MIN_MATCH && start & ((1 << step) - 1) == 0 {
            let w = state.window.filled();
            let h = hash8(load8(w, start));
            let cand = state.head.as_slice()[h] as usize;
            state.head.as_mut_slice()[h] = start as u16;
            let dist = start.wrapping_sub(cand);
            if cand != 0 && dist > 0 && dist <= state.max_dist() {
                let max = state.lookahead.min(STD_MAX_MATCH);
                let len = common(w, cand, start, max);
                // One-step lazy evaluation: a longer match starting at the
                // next byte wins (the current byte goes out as a literal).
                let lazy = len >= IMAGE_MIN_MATCH
                    && len < max
                    && state.lookahead > IMAGE_MIN_MATCH + 1
                    && {
                        let n = start + 1;
                        let c2 = state.head.as_slice()[hash8(load8(w, n))] as usize;
                        let d2 = n.wrapping_sub(c2);
                        c2 != 0
                            && d2 > 0
                            && d2 <= state.max_dist()
                            && common(w, c2, n, (max - 1).min(state.lookahead - 1)) > len + 1
                    };
                if !lazy && len >= IMAGE_MIN_MATCH && worth_it(state, start, len, dist) {
                    misses = 0;
                    let bflush = state.tally_dist(dist, len - STD_MIN_MATCH);
                    state.lookahead -= len;
                    state.strstart += len;
                    // Index the tail of the match so the next row (or the
                    // continuation of a run) finds it.
                    let end = state.strstart;
                    if state.lookahead >= IMAGE_MIN_MATCH {
                        let w = state.window.filled();
                        for p in end.saturating_sub(4).max(start + 1)..end {
                            let h = hash8(load8(w, p));
                            state.head.as_mut_slice()[h] = p as u16;
                        }
                    }
                    if bflush {
                        flush_block!(stream, false);
                    }
                    continue;
                }
            }
        }
        misses += 1;
        let lc = state.window.filled()[start];
        let bflush = state.tally_lit(lc);
        state.lookahead -= 1;
        state.strstart += 1;
        if bflush {
            flush_block!(stream, false);
        }
    }

    // Nothing for fill_window to re-insert: this strategy keeps its own
    // (8-byte) hashes in `head` and no chains.
    stream.state.insert = 0;

    if flush == DeflateFlush::Finish {
        flush_block!(stream, true);
        return BlockState::FinishDone;
    }

    if !stream.state.sym_buf.is_empty() {
        flush_block!(stream, false);
    }

    BlockState::BlockDone
}
