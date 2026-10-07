//! libstdc++'s `std::sort` and `std::nth_element`. Neither is stable, and
//! Tesseract sometimes depends on how they order equal keys, so the port
//! reproduces the exact element moves of GCC's implementation.

const S_THRESHOLD: usize = 16;

fn lg(n: usize) -> usize {
    (usize::BITS - 1 - n.leading_zeros()) as usize
}

fn move_median_to_first<T>(
    v: &mut [T],
    result: usize,
    a: usize,
    b: usize,
    c: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    if less(&v[a], &v[b]) {
        if less(&v[b], &v[c]) {
            v.swap(result, b);
        } else if less(&v[a], &v[c]) {
            v.swap(result, c);
        } else {
            v.swap(result, a);
        }
    } else if less(&v[a], &v[c]) {
        v.swap(result, a);
    } else if less(&v[b], &v[c]) {
        v.swap(result, c);
    } else {
        v.swap(result, b);
    }
}

fn unguarded_partition<T>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    pivot: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) -> usize {
    loop {
        while less(&v[first], &v[pivot]) {
            first += 1;
        }
        last -= 1;
        while less(&v[pivot], &v[last]) {
            last -= 1;
        }
        if first >= last {
            return first;
        }
        v.swap(first, last);
        first += 1;
    }
}

fn unguarded_partition_pivot<T>(
    v: &mut [T],
    first: usize,
    last: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) -> usize {
    let mid = first + (last - first) / 2;
    move_median_to_first(v, first, first + 1, mid, last - 1, less);
    unguarded_partition(v, first + 1, last, first, less)
}

fn unguarded_linear_insert<T: Clone>(
    v: &mut [T],
    mut last: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    let val = v[last].clone();
    let mut next = last - 1;
    while less(&val, &v[next]) {
        v[last] = v[next].clone();
        last = next;
        next -= 1;
    }
    v[last] = val;
}

fn insertion_sort<T: Clone>(
    v: &mut [T],
    first: usize,
    last: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    if first == last {
        return;
    }
    for i in first + 1..last {
        if less(&v[i], &v[first]) {
            let val = v[i].clone();
            for k in (first..i).rev() {
                v[k + 1] = v[k].clone();
            }
            v[first] = val;
        } else {
            unguarded_linear_insert(v, i, less);
        }
    }
}

fn push_heap<T: Clone>(
    v: &mut [T],
    first: usize,
    mut hole: usize,
    top: usize,
    value: T,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    let mut parent = hole.wrapping_sub(1) / 2;
    while hole > top && less(&v[first + parent], &value) {
        v[first + hole] = v[first + parent].clone();
        hole = parent;
        parent = hole.wrapping_sub(1) / 2;
    }
    v[first + hole] = value;
}

fn adjust_heap<T: Clone>(
    v: &mut [T],
    first: usize,
    mut hole: usize,
    len: usize,
    value: T,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    let top = hole;
    let mut second = hole;
    while len >= 1 && second < (len - 1) / 2 {
        second = 2 * (second + 1);
        if less(&v[first + second], &v[first + second - 1]) {
            second -= 1;
        }
        v[first + hole] = v[first + second].clone();
        hole = second;
    }
    if len & 1 == 0 && len >= 2 && second == (len - 2) / 2 {
        second = 2 * (second + 1);
        v[first + hole] = v[first + second - 1].clone();
        hole = second - 1;
    }
    push_heap(v, first, hole, top, value, less);
}

fn make_heap<T: Clone>(
    v: &mut [T],
    first: usize,
    last: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    if last - first < 2 {
        return;
    }
    let len = last - first;
    let mut parent = (len - 2) / 2;
    loop {
        let value = v[first + parent].clone();
        adjust_heap(v, first, parent, len, value, less);
        if parent == 0 {
            return;
        }
        parent -= 1;
    }
}

fn pop_heap<T: Clone>(
    v: &mut [T],
    first: usize,
    last: usize,
    result: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    let value = v[result].clone();
    v[result] = v[first].clone();
    adjust_heap(v, first, 0, last - first, value, less);
}

fn heap_select<T: Clone>(
    v: &mut [T],
    first: usize,
    middle: usize,
    last: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    make_heap(v, first, middle, less);
    for i in middle..last {
        if less(&v[i], &v[first]) {
            pop_heap(v, first, middle, i, less);
        }
    }
}

fn sort_heap<T: Clone>(
    v: &mut [T],
    first: usize,
    mut last: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    while last - first > 1 {
        last -= 1;
        pop_heap(v, first, last, last, less);
    }
}

fn introsort_loop<T: Clone>(
    v: &mut [T],
    first: usize,
    mut last: usize,
    mut depth: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    while last - first > S_THRESHOLD {
        if depth == 0 {
            heap_select(v, first, last, last, less);
            sort_heap(v, first, last, less);
            return;
        }
        depth -= 1;
        let cut = unguarded_partition_pivot(v, first, last, less);
        introsort_loop(v, cut, last, depth, less);
        last = cut;
    }
}

/// `std::sort(v.begin(), v.end(), less)`.
pub fn sort<T: Clone>(v: &mut [T], mut less: impl FnMut(&T, &T) -> bool) {
    let n = v.len();
    if n == 0 {
        return;
    }
    introsort_loop(v, 0, n, lg(n) * 2, &mut less);
    if n > S_THRESHOLD {
        insertion_sort(v, 0, S_THRESHOLD, &mut less);
        for i in S_THRESHOLD..n {
            unguarded_linear_insert(v, i, &mut less);
        }
    } else {
        insertion_sort(v, 0, n, &mut less);
    }
}

/// `std::nth_element(v.begin(), v.begin() + nth, v.end(), less)`.
pub fn nth_element<T: Clone>(v: &mut [T], nth: usize, mut less: impl FnMut(&T, &T) -> bool) {
    let n = v.len();
    if n == 0 || nth == n {
        return;
    }
    let (mut first, mut last) = (0, n);
    let mut depth = lg(n) * 2;
    while last - first > 3 {
        if depth == 0 {
            heap_select(v, first, nth + 1, last, &mut less);
            v.swap(first, nth);
            return;
        }
        depth -= 1;
        let cut = unguarded_partition_pivot(v, first, last, &mut less);
        if cut <= nth {
            first = cut;
        } else {
            last = cut;
        }
    }
    insertion_sort(v, first, last, &mut less);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_and_selects() {
        let mut s = 12345u32;
        let mut v: Vec<u32> = (0..500)
            .map(|_| {
                s = s.wrapping_mul(1103515245).wrapping_add(12345);
                (s >> 16) % 50
            })
            .collect();
        let mut w = v.clone();
        sort(&mut v, |a, b| a < b);
        let mut r = w.clone();
        r.sort();
        assert_eq!(v, r);
        nth_element(&mut w, 250, |a, b| a < b);
        assert_eq!(w[250], r[250]);
        assert!(w[..250].iter().all(|&x| x <= w[250]));
    }
}
