//! `GridBase`, `IntGrid`, `BBGrid` and `GridSearch` (`bbgrid.h`). Cells hold
//! sorted lists of element handles; boxes are looked up through [`BoxOf`].

use super::elist::{EList, Iter};
use super::geom::{ICoord, TBox};
use std::collections::HashSet;
use std::hash::Hash;

/// Bounding boxes of grid elements.
pub trait BoxOf<T> {
    fn box_of(&self, t: T) -> TBox;
}

#[derive(Clone, Debug, Default)]
pub struct GridBase {
    pub gridsize: i32,
    pub gridwidth: i32,
    pub gridheight: i32,
    pub gridbuckets: i32,
    pub bleft: ICoord,
    pub tright: ICoord,
}

impl GridBase {
    pub fn new(gridsize: i32, bleft: ICoord, tright: ICoord) -> GridBase {
        let gridsize = if gridsize == 0 { 1 } else { gridsize };
        let gridwidth = (tright.x - bleft.x + gridsize - 1) / gridsize;
        let gridheight = (tright.y - bleft.y + gridsize - 1) / gridsize;
        GridBase {
            gridsize,
            gridwidth,
            gridheight,
            gridbuckets: gridwidth * gridheight,
            bleft,
            tright,
        }
    }

    pub fn grid_coords(&self, x: i32, y: i32) -> (i32, i32) {
        self.clip_grid_coords(
            (x - self.bleft.x) / self.gridsize,
            (y - self.bleft.y) / self.gridsize,
        )
    }

    pub fn clip_grid_coords(&self, x: i32, y: i32) -> (i32, i32) {
        (
            x.clamp(0, (self.gridwidth - 1).max(0))
                .min(self.gridwidth - 1),
            y.clamp(0, (self.gridheight - 1).max(0))
                .min(self.gridheight - 1),
        )
    }
}

/// `IntGrid`.
#[derive(Clone, Debug, Default)]
pub struct IntGrid {
    pub base: GridBase,
    grid: Vec<i32>,
}

impl IntGrid {
    pub fn new(gridsize: i32, bleft: ICoord, tright: ICoord) -> IntGrid {
        let base = GridBase::new(gridsize, bleft, tright);
        let n = base.gridbuckets.max(0) as usize;
        IntGrid {
            base,
            grid: vec![0; n],
        }
    }

    pub fn clear(&mut self) {
        self.grid.fill(0);
    }

    pub fn cell(&self, x: i32, y: i32) -> i32 {
        let (x, y) = self.base.clip_grid_coords(x, y);
        self.grid[(y * self.base.gridwidth + x) as usize]
    }

    pub fn set_cell(&mut self, x: i32, y: i32, v: i32) {
        self.grid[(y * self.base.gridwidth + x) as usize] = v;
    }

    pub fn neighbourhood_sum(&self) -> IntGrid {
        let b = &self.base;
        let mut sum = IntGrid::new(b.gridsize, b.bleft, b.tright);
        for y in 0..b.gridheight {
            for x in 0..b.gridwidth {
                let mut count = 0;
                for yo in -1..=1 {
                    for xo in -1..=1 {
                        let (gx, gy) = b.clip_grid_coords(x + xo, y + yo);
                        count += self.cell(gx, gy);
                    }
                }
                if self.cell(x, y) > 1 {
                    sum.set_cell(x, y, count);
                }
            }
        }
        sum
    }

    pub fn rect_mostly_over_threshold(&self, rect: &TBox, threshold: i32) -> bool {
        let (min_x, min_y) = self.base.grid_coords(rect.left, rect.bottom);
        let (max_x, max_y) = self.base.grid_coords(rect.right, rect.top);
        let gs = self.base.gridsize;
        let mut total = 0;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                if self.cell(x, y) > threshold {
                    let cell = TBox::new(x * gs, y * gs, (x + 1) * gs, (y + 1) * gs);
                    let c = if cell.overlap(rect) {
                        cell.intersection(rect)
                    } else {
                        TBox::new(
                            i32::from(i16::MAX),
                            i32::from(i16::MAX),
                            -i32::from(i16::MAX),
                            -i32::from(i16::MAX),
                        )
                    };
                    total += c.area();
                }
            }
        }
        total * 2 > rect.area()
    }

    pub fn any_zero_in_rect(&self, rect: &TBox) -> bool {
        let (min_x, min_y) = self.base.grid_coords(rect.left, rect.bottom);
        let (max_x, max_y) = self.base.grid_coords(rect.right, rect.top);
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                if self.cell(x, y) == 0 {
                    return true;
                }
            }
        }
        false
    }

    /// `ThresholdToPix`.
    pub fn threshold_to_pix(&self, threshold: i32) -> super::bitmap::Bitmap {
        let b = &self.base;
        let mut pix = super::bitmap::Bitmap::new(
            (b.tright.x - b.bleft.x) as usize,
            (b.tright.y - b.bleft.y) as usize,
        );
        let cs = b.gridsize;
        for y in 0..b.gridheight {
            for x in 0..b.gridwidth {
                if self.cell(x, y) > threshold
                    && self.cell(x - 1, y) > 0
                    && self.cell(x + 1, y) > 0
                    && self.cell(x, y - 1) > 0
                    && self.cell(x, y + 1) > 0
                {
                    pix.set_rect(&super::morph::LBox {
                        x: x * cs,
                        y: b.tright.y - (y + 1) * cs,
                        w: cs,
                        h: cs,
                    });
                }
            }
        }
        pix
    }
}

/// `SortByBoxLeft`.
pub fn sort_by_box_left(a: &TBox, b: &TBox) -> i32 {
    let r = a.left - b.left;
    if r != 0 {
        return r;
    }
    let r = a.right - b.right;
    if r != 0 {
        return r;
    }
    let r = a.bottom - b.bottom;
    if r != 0 {
        return r;
    }
    a.top - b.top
}

/// `SortRightToLeft`.
pub fn sort_right_to_left(a: &TBox, b: &TBox) -> i32 {
    let r = b.right - a.right;
    if r != 0 {
        return r;
    }
    let r = b.left - a.left;
    if r != 0 {
        return r;
    }
    let r = a.bottom - b.bottom;
    if r != 0 {
        return r;
    }
    a.top - b.top
}

/// `SortByBoxBottom`.
pub fn sort_by_box_bottom(a: &TBox, b: &TBox) -> i32 {
    let r = a.bottom - b.bottom;
    if r != 0 {
        return r;
    }
    let r = a.top - b.top;
    if r != 0 {
        return r;
    }
    let r = a.left - b.left;
    if r != 0 {
        return r;
    }
    a.right - b.right
}

/// `CLIST::add_sorted(comparator, unique, data)`.
pub fn clist_add_sorted<T: Copy + PartialEq>(
    list: &mut EList<T>,
    mut cmp: impl FnMut(T, T) -> i32,
    unique: bool,
    v: T,
) -> bool {
    let last = list.to_vec().last().copied();
    match last {
        None => {
            list.push_back(v);
            return true;
        }
        Some(l) if cmp(l, v) < 0 => {
            list.push_back(v);
            return true;
        }
        Some(l) if unique && l == v => return false,
        _ => {}
    }
    let mut it = Iter::new(list);
    it.mark_cycle_pt();
    while !it.cycled_list(list) {
        let d = it.data(list);
        if d == v && unique {
            return false;
        }
        if cmp(d, v) > 0 {
            break;
        }
        it.forward(list);
    }
    if it.cycled_list(list) {
        it.add_to_end(list, v);
    } else {
        it.add_before_then_move(list, v);
    }
    true
}

/// `BBGrid`.
#[derive(Clone, Debug, Default)]
pub struct BBGrid<T> {
    pub base: GridBase,
    pub cells: Vec<EList<T>>,
}

impl<T: Copy + PartialEq + Eq + Hash> BBGrid<T> {
    pub fn new(gridsize: i32, bleft: ICoord, tright: ICoord) -> BBGrid<T> {
        let base = GridBase::new(gridsize, bleft, tright);
        let n = base.gridbuckets.max(0) as usize;
        BBGrid {
            base,
            cells: (0..n).map(|_| EList::new()).collect(),
        }
    }

    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = EList::new();
        }
    }

    pub fn insert_bbox(&mut self, src: &impl BoxOf<T>, h_spread: bool, v_spread: bool, t: T) {
        let b = src.box_of(t);
        let (sx, sy) = self.base.grid_coords(b.left, b.bottom);
        let (mut ex, mut ey) = self.base.grid_coords(b.right, b.top);
        if !h_spread {
            ex = sx;
        }
        if !v_spread {
            ey = sy;
        }
        let w = self.base.gridwidth;
        for y in sy..=ey {
            for x in sx..=ex {
                let cell = &mut self.cells[(y * w + x) as usize];
                clist_add_sorted(
                    cell,
                    |a, b| sort_by_box_left(&src.box_of(a), &src.box_of(b)),
                    true,
                    t,
                );
            }
        }
    }

    /// `InsertPixPtBBox`.
    pub fn insert_pix_pt_bbox(
        &mut self,
        src: &impl BoxOf<T>,
        left: i32,
        bottom: i32,
        pix: &super::bitmap::Bitmap,
        t: T,
    ) {
        let w = self.base.gridwidth;
        for y in 0..pix.height {
            for x in 0..pix.width {
                if pix.get(x, y) {
                    let idx = (bottom + y as i32) * w + x as i32 + left;
                    clist_add_sorted(
                        &mut self.cells[idx as usize],
                        |a, b| sort_by_box_left(&src.box_of(a), &src.box_of(b)),
                        true,
                        t,
                    );
                }
            }
        }
    }

    /// `RemoveBBox` using the given (current) box of `t`.
    pub fn remove_bbox_at(&mut self, b: TBox, t: T) {
        let (sx, sy) = self.base.grid_coords(b.left, b.bottom);
        let (ex, ey) = self.base.grid_coords(b.right, b.top);
        let w = self.base.gridwidth;
        for y in sy..=ey {
            for x in sx..=ex {
                let cell = &mut self.cells[(y * w + x) as usize];
                let mut it = Iter::new(cell);
                it.mark_cycle_pt();
                while !it.cycled_list(cell) {
                    if it.data(cell) == t {
                        it.extract(cell);
                    }
                    it.forward(cell);
                }
            }
        }
    }

    pub fn remove_bbox(&mut self, src: &impl BoxOf<T>, t: T) {
        self.remove_bbox_at(src.box_of(t), t);
    }

    pub fn rectangle_empty(&self, src: &impl BoxOf<T>, rect: &TBox) -> bool {
        let mut s = GridSearch::new();
        s.start_rect_search(self, rect);
        s.next_rect_search(self, src).is_none()
    }

    pub fn count_cell_elements(&self) -> IntGrid {
        let b = &self.base;
        let mut g = IntGrid::new(b.gridsize, b.bleft, b.tright);
        for y in 0..b.gridheight {
            for x in 0..b.gridwidth {
                g.set_cell(
                    x,
                    y,
                    self.cells[(y * b.gridwidth + x) as usize].len() as i32,
                );
            }
        }
        g
    }
}

/// `GridSearch`. The grid is passed to every call.
#[derive(Clone, Debug)]
pub struct GridSearch<T> {
    x_origin: i32,
    y_origin: i32,
    max_radius: i32,
    radius: i32,
    rad_index: i32,
    rad_dir: i32,
    rect: TBox,
    pub x: i32,
    pub y: i32,
    unique_mode: bool,
    previous_return: Option<T>,
    next_return: Option<T>,
    cell: usize,
    it: Iter,
    returns: HashSet<T>,
}

const CHAIN: [ICoord; 4] = [
    ICoord::new(-1, 0),
    ICoord::new(0, -1),
    ICoord::new(1, 0),
    ICoord::new(0, 1),
];

impl<T: Copy + PartialEq + Eq + Hash> Default for GridSearch<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq + Eq + Hash> GridSearch<T> {
    pub fn new() -> GridSearch<T> {
        GridSearch {
            x_origin: 0,
            y_origin: 0,
            max_radius: 0,
            radius: 0,
            rad_index: 0,
            rad_dir: 0,
            rect: TBox::default(),
            x: 0,
            y: 0,
            unique_mode: false,
            previous_return: None,
            next_return: None,
            cell: 0,
            it: Iter::new(&EList::<T>::new()),
            returns: HashSet::new(),
        }
    }

    pub fn set_unique_mode(&mut self, mode: bool) {
        self.unique_mode = mode;
    }

    pub fn returned_seed_element(&self, grid: &BBGrid<T>, src: &impl BoxOf<T>) -> bool {
        let Some(p) = self.previous_return else {
            return false;
        };
        let b = src.box_of(p);
        let (gx, gy) = grid
            .base
            .grid_coords((b.left + b.right) / 2, (b.top + b.bottom) / 2);
        self.x == gx && self.y == gy
    }

    fn cycled(&self, grid: &BBGrid<T>) -> bool {
        self.it.cycled_list(&grid.cells[self.cell])
    }

    fn set_iterator(&mut self, grid: &BBGrid<T>) {
        self.cell = (self.y * grid.base.gridwidth + self.x) as usize;
        self.it = Iter::new(&grid.cells[self.cell]);
        self.it.mark_cycle_pt();
    }

    fn common_start(&mut self, grid: &BBGrid<T>, x: i32, y: i32) {
        let (xo, yo) = grid.base.grid_coords(x, y);
        self.x_origin = xo;
        self.y_origin = yo;
        self.x = xo;
        self.y = yo;
        self.set_iterator(grid);
        self.previous_return = None;
        let cell = &grid.cells[self.cell];
        self.next_return = if cell.is_empty() {
            None
        } else {
            Some(self.it.data(cell))
        };
        self.returns.clear();
    }

    fn common_next(&mut self, grid: &BBGrid<T>) -> T {
        let cell = &grid.cells[self.cell];
        let p = self.it.data(cell);
        self.previous_return = Some(p);
        self.it.forward(cell);
        self.next_return = if self.it.cycled_list(cell) {
            None
        } else {
            Some(self.it.data(cell))
        };
        p
    }

    fn common_end(&mut self) -> Option<T> {
        self.previous_return = None;
        self.next_return = None;
        None
    }

    pub fn start_full_search(&mut self, grid: &BBGrid<T>) {
        self.common_start(grid, grid.base.bleft.x, grid.base.tright.y);
    }

    pub fn next_full_search(&mut self, grid: &BBGrid<T>, src: &impl BoxOf<T>) -> Option<T> {
        loop {
            while self.cycled(grid) {
                self.x += 1;
                if self.x >= grid.base.gridwidth {
                    self.y -= 1;
                    if self.y < 0 {
                        return self.common_end();
                    }
                    self.x = 0;
                }
                self.set_iterator(grid);
            }
            let p = self.common_next(grid);
            let b = src.box_of(p);
            let (x, y) = grid.base.grid_coords(b.left, b.bottom);
            if x == self.x && y == self.y {
                return Some(p);
            }
        }
    }

    pub fn start_rad_search(&mut self, grid: &BBGrid<T>, x: i32, y: i32, max_radius: i32) {
        self.max_radius = max_radius;
        self.radius = 0;
        self.rad_index = 0;
        self.rad_dir = 3;
        self.common_start(grid, x, y);
    }

    pub fn next_rad_search(&mut self, grid: &BBGrid<T>) -> Option<T> {
        loop {
            while self.cycled(grid) {
                self.rad_index += 1;
                if self.rad_index >= self.radius {
                    self.rad_dir += 1;
                    self.rad_index = 0;
                    if self.rad_dir >= 4 {
                        self.radius += 1;
                        if self.radius > self.max_radius {
                            return self.common_end();
                        }
                        self.rad_dir = 0;
                    }
                }
                let s0 = CHAIN[(self.rad_dir & 3) as usize];
                let s1 = CHAIN[((self.rad_dir + 1) & 3) as usize];
                let k = self.radius - self.rad_index;
                let ox = s0.x * k + s1.x * self.rad_index;
                let oy = s0.y * k + s1.y * self.rad_index;
                self.x = self.x_origin + ox;
                self.y = self.y_origin + oy;
                if self.x >= 0
                    && self.x < grid.base.gridwidth
                    && self.y >= 0
                    && self.y < grid.base.gridheight
                {
                    self.set_iterator(grid);
                }
            }
            let p = self.common_next(grid);
            if !self.unique_mode || self.returns.insert(p) {
                return Some(p);
            }
        }
    }

    pub fn start_side_search(&mut self, grid: &BBGrid<T>, x: i32, ymin: i32, ymax: i32) {
        self.radius = ((ymax - ymin) * 2 + grid.base.gridsize - 1) / grid.base.gridsize;
        self.rad_index = 0;
        self.common_start(grid, x, ymax);
    }

    pub fn next_side_search(&mut self, grid: &BBGrid<T>, right_to_left: bool) -> Option<T> {
        loop {
            while self.cycled(grid) {
                self.rad_index += 1;
                if self.rad_index > self.radius {
                    if right_to_left {
                        self.x -= 1;
                    } else {
                        self.x += 1;
                    }
                    self.rad_index = 0;
                    if self.x < 0 || self.x >= grid.base.gridwidth {
                        return self.common_end();
                    }
                }
                self.y = self.y_origin - self.rad_index;
                if self.y >= 0 && self.y < grid.base.gridheight {
                    self.set_iterator(grid);
                }
            }
            let p = self.common_next(grid);
            if !self.unique_mode || self.returns.insert(p) {
                return Some(p);
            }
        }
    }

    pub fn start_vertical_search(&mut self, grid: &BBGrid<T>, xmin: i32, xmax: i32, y: i32) {
        self.radius = (xmax - xmin + grid.base.gridsize - 1) / grid.base.gridsize;
        self.rad_index = 0;
        self.common_start(grid, xmin, y);
    }

    pub fn next_vertical_search(&mut self, grid: &BBGrid<T>, top_to_bottom: bool) -> Option<T> {
        loop {
            while self.cycled(grid) {
                self.rad_index += 1;
                if self.rad_index > self.radius {
                    if top_to_bottom {
                        self.y -= 1;
                    } else {
                        self.y += 1;
                    }
                    self.rad_index = 0;
                    if self.y < 0 || self.y >= grid.base.gridheight {
                        return self.common_end();
                    }
                }
                self.x = self.x_origin + self.rad_index;
                if self.x >= 0 && self.x < grid.base.gridwidth {
                    self.set_iterator(grid);
                }
            }
            let p = self.common_next(grid);
            if !self.unique_mode || self.returns.insert(p) {
                return Some(p);
            }
        }
    }

    pub fn start_rect_search(&mut self, grid: &BBGrid<T>, rect: &TBox) {
        self.rect = *rect;
        self.common_start(grid, rect.left, rect.top);
        let (mx, yo) = grid.base.grid_coords(rect.right, rect.bottom);
        self.max_radius = mx;
        self.y_origin = yo;
    }

    pub fn next_rect_search(&mut self, grid: &BBGrid<T>, src: &impl BoxOf<T>) -> Option<T> {
        loop {
            while self.cycled(grid) {
                self.x += 1;
                if self.x > self.max_radius {
                    self.y -= 1;
                    self.x = self.x_origin;
                    if self.y < self.y_origin {
                        return self.common_end();
                    }
                }
                self.set_iterator(grid);
            }
            let p = self.common_next(grid);
            if !self.rect.overlap(&src.box_of(p)) {
                continue;
            }
            if !self.unique_mode || self.returns.insert(p) {
                return Some(p);
            }
        }
    }

    /// `GridSearch::RemoveBBox`: removes the previous return from the grid,
    /// using its current box.
    pub fn remove_bbox(&mut self, grid: &mut BBGrid<T>, src: &impl BoxOf<T>) {
        let Some(prev) = self.previous_return else {
            return;
        };
        let mut prev_data = None;
        let mut new_previous = None;
        {
            let cell = &mut grid.cells[self.cell];
            self.it.move_to_first(cell);
            self.it.mark_cycle_pt();
            while !self.it.cycled_list(cell) {
                let d = self.it.data(cell);
                if d == prev {
                    new_previous = prev_data;
                    self.it.extract(cell);
                    self.it.forward(cell);
                    self.next_return = if self.it.cycled_list(cell) {
                        None
                    } else {
                        Some(self.it.data(cell))
                    };
                } else {
                    prev_data = Some(d);
                    self.it.forward(cell);
                }
            }
        }
        grid.remove_bbox(src, prev);
        self.previous_return = new_previous;
        self.reposition_iterator(grid);
    }

    pub fn reposition_iterator(&mut self, grid: &BBGrid<T>) {
        self.returns.clear();
        let cell = &grid.cells[self.cell];
        self.it.move_to_first(cell);
        if !cell.is_empty() && Some(self.it.data(cell)) == self.next_return {
            self.it.mark_cycle_pt();
            return;
        }
        self.it.mark_cycle_pt();
        while !self.it.cycled_list(cell) {
            if Some(self.it.data(cell)) == self.previous_return
                || Some(self.it.data_relative(cell, 1)) == self.next_return
            {
                self.common_next(grid);
                return;
            }
            self.it.forward(cell);
        }
        self.previous_return = None;
        self.next_return = None;
    }

    pub fn previous_return(&self) -> Option<T> {
        self.previous_return
    }
}
