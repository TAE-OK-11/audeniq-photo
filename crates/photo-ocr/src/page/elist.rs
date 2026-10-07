//! Tesseract's `ELIST` / `ELIST_ITERATOR`: circular singly linked lists whose
//! iterators survive extraction of the current element. The textord code
//! relies on the exact element order these produce (insertions after an
//! extracted element, cycle points that move on deletion), so the port keeps
//! the original pointer logic over a node slab. Elements are small copyable
//! handles (arena ids).

const NIL: usize = usize::MAX;

#[derive(Clone, Debug)]
struct Node<T> {
    val: T,
    next: usize,
    prev: usize,
}

#[derive(Clone, Debug)]
pub struct EList<T> {
    nodes: Vec<Node<T>>,
    last: usize,
}

impl<T: Copy> Default for EList<T> {
    fn default() -> Self {
        EList {
            nodes: Vec::new(),
            last: NIL,
        }
    }
}

impl<T: Copy> EList<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.last == NIL
    }

    fn first(&self) -> usize {
        if self.last == NIL {
            NIL
        } else {
            self.nodes[self.last].next
        }
    }

    // Nodes are never reused: an iterator may still hold an extracted
    // node as its cycle point, which must not alias a new element.
    fn alloc(&mut self, val: T) -> usize {
        self.nodes.push(Node {
            val,
            next: NIL,
            prev: NIL,
        });
        self.nodes.len() - 1
    }

    pub fn len(&self) -> usize {
        let mut n = 0;
        if self.last != NIL {
            let mut p = self.first();
            loop {
                n += 1;
                if p == self.last {
                    break;
                }
                p = self.nodes[p].next;
            }
        }
        n
    }

    /// Elements in list order.
    pub fn to_vec(&self) -> Vec<T> {
        let mut out = Vec::new();
        if self.last != NIL {
            let mut p = self.first();
            loop {
                out.push(self.nodes[p].val);
                if p == self.last {
                    break;
                }
                p = self.nodes[p].next;
            }
        }
        out
    }

    pub fn from_vec(vals: &[T]) -> Self {
        let mut l = EList::new();
        for &v in vals {
            l.push_back(v);
        }
        l
    }

    /// `ELIST_ITERATOR(list).add_to_end(v)` on a fresh iterator.
    pub fn push_back(&mut self, val: T) {
        let n = self.alloc(val);
        if self.last == NIL {
            self.nodes[n].next = n;
            self.nodes[n].prev = n;
        } else {
            let first = self.nodes[self.last].next;
            self.link(n, first);
            self.link(self.last, n);
        }
        self.last = n;
    }

    /// Sets `a.next = b` and `b.prev = a`.
    fn link(&mut self, a: usize, b: usize) {
        self.nodes[a].next = b;
        self.nodes[b].prev = a;
    }

    /// Removes and returns every element (the list becomes empty).
    pub fn take_all(&mut self) -> Vec<T> {
        let v = self.to_vec();
        *self = EList::new();
        v
    }

    /// `ELIST::sort`: extract into an array, `qsort` it (glibc: stable
    /// mergesort), and rebuild.
    pub fn sort_by(&mut self, cmp: impl FnMut(&T, &T) -> std::cmp::Ordering) {
        let mut v = self.take_all();
        v.sort_by(cmp);
        for x in v {
            self.push_back(x);
        }
    }

    /// `ELIST::add_sorted_and_find` with `unique` false.
    pub fn add_sorted(&mut self, mut cmp: impl FnMut(&T, &T) -> std::cmp::Ordering, val: T) {
        use std::cmp::Ordering::Less;
        if self.last == NIL || cmp(&self.nodes[self.last].val, &val) == Less {
            self.push_back(val);
            return;
        }
        let mut it = Iter::new(self);
        it.mark_cycle_pt();
        while !it.cycled_list(self) {
            if cmp(&it.data(self), &val) == std::cmp::Ordering::Greater {
                break;
            }
            it.forward(self);
        }
        if it.cycled_list(self) {
            it.add_to_end(self, val);
        } else {
            it.add_before_then_move(self, val);
        }
    }
}

/// `ELIST_ITERATOR` / `ELIST2_ITERATOR`. The list is passed to every call.
#[derive(Clone, Copy, Debug)]
pub struct Iter {
    prev: usize,
    current: usize,
    next: usize,
    cycle_pt: usize,
    ex_current_was_last: bool,
    ex_current_was_cycle_pt: bool,
    started_cycling: bool,
}

impl Iter {
    /// `set_to_list`.
    pub fn new<T: Copy>(list: &EList<T>) -> Iter {
        let current = list.first();
        Iter {
            prev: list.last,
            current,
            next: if current == NIL {
                NIL
            } else {
                list.nodes[current].next
            },
            cycle_pt: NIL,
            started_cycling: false,
            ex_current_was_last: false,
            ex_current_was_cycle_pt: false,
        }
    }

    pub fn data<T: Copy>(&self, list: &EList<T>) -> T {
        list.nodes[self.current].val
    }

    pub fn data_mut<'a, T: Copy>(&self, list: &'a mut EList<T>) -> &'a mut T {
        &mut list.nodes[self.current].val
    }

    pub fn current_extracted(&self) -> bool {
        self.current == NIL
    }

    pub fn forward<T: Copy>(&mut self, list: &EList<T>) -> Option<T> {
        if list.is_empty() {
            return None;
        }
        if self.current != NIL {
            self.prev = self.current;
            self.started_cycling = true;
            self.current = list.nodes[self.current].next;
        } else {
            if self.ex_current_was_cycle_pt {
                self.cycle_pt = self.next;
            }
            self.current = self.next;
        }
        self.next = list.nodes[self.current].next;
        Some(list.nodes[self.current].val)
    }

    /// `ELIST2_ITERATOR::backward`.
    pub fn backward<T: Copy>(&mut self, list: &EList<T>) -> Option<T> {
        if list.is_empty() {
            return None;
        }
        if self.current != NIL {
            self.next = self.current;
            self.started_cycling = true;
            self.current = list.nodes[self.current].prev;
        } else {
            if self.ex_current_was_cycle_pt {
                self.cycle_pt = self.prev;
            }
            self.current = self.prev;
        }
        self.prev = list.nodes[self.current].prev;
        Some(list.nodes[self.current].val)
    }

    /// `data_relative(offset)`; negative offsets walk backwards (ELIST2).
    pub fn data_relative<T: Copy>(&self, list: &EList<T>, offset: i32) -> T {
        if offset == -1 {
            return list.nodes[self.prev].val;
        }
        let mut p = if self.current != NIL {
            self.current
        } else {
            self.prev
        };
        if offset < 0 {
            for _ in 0..-offset {
                p = list.nodes[p].prev;
            }
        } else {
            for _ in 0..offset {
                p = list.nodes[p].next;
            }
        }
        list.nodes[p].val
    }

    pub fn extract<T: Copy>(&mut self, list: &mut EList<T>) -> T {
        let cur = self.current;
        if list.last != NIL && list.nodes[list.last].next == list.last {
            self.prev = NIL;
            self.next = NIL;
            list.last = NIL;
        } else {
            list.link(self.prev, self.next);
            self.ex_current_was_last = cur == list.last;
            if self.ex_current_was_last {
                list.last = self.prev;
            }
        }
        self.ex_current_was_cycle_pt = cur == self.cycle_pt;
        self.current = NIL;
        list.nodes[cur].val
    }

    pub fn move_to_first<T: Copy>(&mut self, list: &EList<T>) -> Option<T> {
        self.current = list.first();
        self.prev = list.last;
        self.next = if self.current == NIL {
            NIL
        } else {
            list.nodes[self.current].next
        };
        (self.current != NIL).then(|| list.nodes[self.current].val)
    }

    /// `ELIST_ITERATOR::move_to_last` (walks forward).
    pub fn move_to_last<T: Copy>(&mut self, list: &EList<T>) -> Option<T> {
        while self.current != list.last {
            self.forward(list);
        }
        (self.current != NIL).then(|| list.nodes[self.current].val)
    }

    /// `ELIST2_ITERATOR::move_to_last` (jumps).
    pub fn move_to_last2<T: Copy>(&mut self, list: &EList<T>) -> Option<T> {
        self.current = list.last;
        if self.current == NIL {
            self.prev = NIL;
            self.next = NIL;
            return None;
        }
        self.prev = list.nodes[self.current].prev;
        self.next = list.nodes[self.current].next;
        Some(list.nodes[self.current].val)
    }

    pub fn mark_cycle_pt(&mut self) {
        if self.current != NIL {
            self.cycle_pt = self.current;
        } else {
            self.ex_current_was_cycle_pt = true;
        }
        self.started_cycling = false;
    }

    pub fn cycled_list<T: Copy>(&self, list: &EList<T>) -> bool {
        list.is_empty() || (self.current == self.cycle_pt && self.started_cycling)
    }

    pub fn at_first<T: Copy>(&self, list: &EList<T>) -> bool {
        list.is_empty()
            || self.current == list.first()
            || (self.current == NIL && self.prev == list.last && !self.ex_current_was_last)
    }

    pub fn at_last<T: Copy>(&self, list: &EList<T>) -> bool {
        list.is_empty()
            || self.current == list.last
            || (self.current == NIL && self.prev == list.last && self.ex_current_was_last)
    }

    pub fn add_after_then_move<T: Copy>(&mut self, list: &mut EList<T>, val: T) {
        let n = list.alloc(val);
        if list.is_empty() {
            list.link(n, n);
            list.last = n;
            self.prev = n;
            self.next = n;
        } else {
            list.link(n, self.next);
            if self.current != NIL {
                list.link(self.current, n);
                self.prev = self.current;
                if self.current == list.last {
                    list.last = n;
                }
            } else {
                list.link(self.prev, n);
                if self.ex_current_was_last {
                    list.last = n;
                }
                if self.ex_current_was_cycle_pt {
                    self.cycle_pt = n;
                }
            }
        }
        self.current = n;
    }

    pub fn add_after_stay_put<T: Copy>(&mut self, list: &mut EList<T>, val: T) {
        let n = list.alloc(val);
        if list.is_empty() {
            list.link(n, n);
            list.last = n;
            self.prev = n;
            self.next = n;
            self.ex_current_was_last = false;
            self.current = NIL;
        } else {
            list.link(n, self.next);
            if self.current != NIL {
                list.link(self.current, n);
                if self.prev == self.current {
                    self.prev = n;
                }
                if self.current == list.last {
                    list.last = n;
                }
            } else {
                list.link(self.prev, n);
                if self.ex_current_was_last {
                    list.last = n;
                    self.ex_current_was_last = false;
                }
            }
            self.next = n;
        }
    }

    pub fn add_before_then_move<T: Copy>(&mut self, list: &mut EList<T>, val: T) {
        let n = list.alloc(val);
        if list.is_empty() {
            list.link(n, n);
            list.last = n;
            self.prev = n;
            self.next = n;
        } else {
            list.link(self.prev, n);
            if self.current != NIL {
                list.link(n, self.current);
                self.next = self.current;
            } else {
                list.link(n, self.next);
                if self.ex_current_was_last {
                    list.last = n;
                }
                if self.ex_current_was_cycle_pt {
                    self.cycle_pt = n;
                }
            }
        }
        self.current = n;
    }

    pub fn add_before_stay_put<T: Copy>(&mut self, list: &mut EList<T>, val: T) {
        let n = list.alloc(val);
        if list.is_empty() {
            list.link(n, n);
            list.last = n;
            self.prev = n;
            self.next = n;
            self.ex_current_was_last = true;
            self.current = NIL;
        } else {
            list.link(self.prev, n);
            if self.current != NIL {
                list.link(n, self.current);
                if self.next == self.current {
                    self.next = n;
                }
            } else {
                list.link(n, self.next);
                if self.ex_current_was_last {
                    list.last = n;
                }
            }
            self.prev = n;
        }
    }

    fn chain<T: Copy>(list: &mut EList<T>, vals: &[T]) -> (usize, usize) {
        let ids: Vec<usize> = vals.iter().map(|&v| list.alloc(v)).collect();
        for w in ids.windows(2) {
            list.link(w[0], w[1]);
        }
        (ids[0], ids[ids.len() - 1])
    }

    /// Moves the elements of `vals` (in order) into the list after the
    /// current element, without moving (`add_list_after`).
    pub fn add_list_after<T: Copy>(&mut self, list: &mut EList<T>, vals: &[T]) {
        if vals.is_empty() {
            return;
        }
        let (first, last) = Self::chain(list, vals);
        if list.is_empty() {
            list.link(last, first);
            list.last = last;
            self.prev = last;
            self.next = first;
            self.ex_current_was_last = true;
            self.current = NIL;
        } else if self.current != NIL {
            let nx = self.next;
            list.link(self.current, first);
            if self.current == list.last {
                list.last = last;
            }
            list.link(last, nx);
            self.next = first;
        } else {
            let nx = self.next;
            list.link(self.prev, first);
            if self.ex_current_was_last {
                list.last = last;
                self.ex_current_was_last = false;
            }
            list.link(last, nx);
            self.next = first;
        }
    }

    /// `add_list_before`: inserts before current and moves to the first
    /// inserted element.
    pub fn add_list_before<T: Copy>(&mut self, list: &mut EList<T>, vals: &[T]) {
        if vals.is_empty() {
            return;
        }
        let (first, last) = Self::chain(list, vals);
        if list.is_empty() {
            list.link(last, first);
            list.last = last;
            self.prev = last;
            self.current = first;
            self.next = list.nodes[first].next;
            self.ex_current_was_last = false;
        } else {
            list.link(self.prev, first);
            if self.current != NIL {
                list.link(last, self.current);
            } else {
                list.link(last, self.next);
                if self.ex_current_was_last {
                    list.last = last;
                }
                if self.ex_current_was_cycle_pt {
                    self.cycle_pt = first;
                }
            }
            self.current = first;
            self.next = list.nodes[first].next;
        }
    }

    pub fn add_to_end<T: Copy>(&mut self, list: &mut EList<T>, val: T) {
        if self.at_last(list) {
            self.add_after_stay_put(list, val);
        } else if self.at_first(list) {
            self.add_before_stay_put(list, val);
            list.last = self.prev;
        } else {
            let n = list.alloc(val);
            let first = list.nodes[list.last].next;
            list.link(n, first);
            list.link(list.last, n);
            list.last = n;
        }
    }

    /// `ELIST_ITERATOR::sort`.
    pub fn sort<T: Copy>(
        &mut self,
        list: &mut EList<T>,
        cmp: impl FnMut(&T, &T) -> std::cmp::Ordering,
    ) {
        list.sort_by(cmp);
        self.move_to_first(list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_then_add_reinserts_in_place() {
        let mut l = EList::from_vec(&[1, 2, 3, 4]);
        let mut it = Iter::new(&l);
        it.forward(&l);
        assert_eq!(it.extract(&mut l), 2);
        it.add_after_then_move(&mut l, 9);
        assert_eq!(l.to_vec(), vec![1, 9, 3, 4]);
        let mut it = Iter::new(&l);
        it.mark_cycle_pt();
        let mut seen = Vec::new();
        while !it.cycled_list(&l) {
            let v = it.data(&l);
            if v % 2 == 1 {
                it.extract(&mut l);
            } else {
                seen.push(v);
            }
            it.forward(&l);
        }
        assert_eq!(seen, vec![4]);
        assert_eq!(l.to_vec(), vec![4]);
        let mut it = Iter::new(&l);
        it.add_to_end(&mut l, 5);
        it.add_after_then_move(&mut l, 6);
        assert_eq!(l.to_vec(), vec![4, 6, 5]);
        let mut it = Iter::new(&l);
        it.add_list_before(&mut l, &[7, 8]);
        it.forward(&l);
        it.extract(&mut l);
        it.add_list_after(&mut l, &[1, 2]);
        let fwd = l.to_vec();
        assert_eq!(fwd, vec![7, 1, 2, 4, 6, 5]);
        let mut it = Iter::new(&l);
        it.move_to_last2(&l);
        let mut back = vec![it.data(&l)];
        for _ in 1..fwd.len() {
            back.push(it.backward(&l).unwrap());
        }
        back.reverse();
        assert_eq!(back, fwd);
    }
}
