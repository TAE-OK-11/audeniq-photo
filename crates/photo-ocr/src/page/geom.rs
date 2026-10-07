//! Tesseract's `ICOORD` / `TBOX` in page coordinates (origin bottom left).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ICoord {
    pub x: i32,
    pub y: i32,
}

impl ICoord {
    pub const fn new(x: i32, y: i32) -> ICoord {
        ICoord { x, y }
    }

    /// `operator*`: the cross product.
    pub fn cross(self, o: ICoord) -> i32 {
        self.x * o.y - self.y * o.x
    }

    pub fn dot(self, o: ICoord) -> i32 {
        self.x * o.x + self.y * o.y
    }

    pub fn sqlength(self) -> f32 {
        (self.x * self.x + self.y * self.y) as f32
    }
}

impl std::ops::Add for ICoord {
    type Output = ICoord;
    fn add(self, o: ICoord) -> ICoord {
        ICoord::new(self.x + o.x, self.y + o.y)
    }
}

impl std::ops::Sub for ICoord {
    type Output = ICoord;
    fn sub(self, o: ICoord) -> ICoord {
        ICoord::new(self.x - o.x, self.y - o.y)
    }
}

impl std::ops::AddAssign for ICoord {
    fn add_assign(&mut self, o: ICoord) {
        self.x += o.x;
        self.y += o.y;
    }
}

const NULL_LO: i32 = i16::MAX as i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TBox {
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    pub top: i32,
}

impl Default for TBox {
    /// The null box (`TBOX()`), the identity for [`TBox::union`].
    fn default() -> TBox {
        TBox {
            left: NULL_LO,
            bottom: NULL_LO,
            right: -NULL_LO,
            top: -NULL_LO,
        }
    }
}

impl TBox {
    pub const fn new(left: i32, bottom: i32, right: i32, top: i32) -> TBox {
        TBox {
            left,
            bottom,
            right,
            top,
        }
    }

    /// `TBOX(pt1, pt2)`: corners in any order.
    pub fn from_corners(a: ICoord, b: ICoord) -> TBox {
        TBox::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
    }

    pub fn botleft(&self) -> ICoord {
        ICoord::new(self.left, self.bottom)
    }

    pub fn topright(&self) -> ICoord {
        ICoord::new(self.right, self.top)
    }

    pub fn null_box(&self) -> bool {
        self.left >= self.right || self.top <= self.bottom
    }

    pub fn width(&self) -> i32 {
        if self.null_box() {
            0
        } else {
            self.right - self.left
        }
    }

    pub fn height(&self) -> i32 {
        if self.null_box() {
            0
        } else {
            self.top - self.bottom
        }
    }

    pub fn area(&self) -> i32 {
        if self.null_box() {
            0
        } else {
            self.width() * self.height()
        }
    }

    /// `operator+=`.
    pub fn union_with(&mut self, o: &TBox) {
        self.left = self.left.min(o.left);
        self.right = self.right.max(o.right);
        self.bottom = self.bottom.min(o.bottom);
        self.top = self.top.max(o.top);
    }

    pub fn bounding_union(&self, o: &TBox) -> TBox {
        let mut b = *self;
        b.union_with(o);
        b
    }

    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        x >= self.left as f32
            && x <= self.right as f32
            && y >= self.bottom as f32
            && y <= self.top as f32
    }

    pub fn contains_pt(&self, p: ICoord) -> bool {
        p.x >= self.left && p.x <= self.right && p.y >= self.bottom && p.y <= self.top
    }

    pub fn contains(&self, b: &TBox) -> bool {
        self.contains_pt(b.botleft()) && self.contains_pt(b.topright())
    }

    pub fn overlap(&self, b: &TBox) -> bool {
        b.left <= self.right && b.right >= self.left && b.bottom <= self.top && b.top >= self.bottom
    }

    pub fn x_overlap(&self, b: &TBox) -> bool {
        b.left <= self.right && b.right >= self.left
    }

    pub fn y_overlap(&self, b: &TBox) -> bool {
        b.bottom <= self.top && b.top >= self.bottom
    }

    pub fn x_gap(&self, b: &TBox) -> i32 {
        self.left.max(b.left) - self.right.min(b.right)
    }

    pub fn y_gap(&self, b: &TBox) -> i32 {
        self.bottom.max(b.bottom) - self.top.min(b.top)
    }

    pub fn major_overlap(&self, b: &TBox) -> bool {
        let mut ov = b.right.min(self.right) - b.left.max(self.left);
        ov += ov;
        if ov < b.width().min(self.width()) {
            return false;
        }
        let mut ov = b.top.min(self.top) - b.bottom.max(self.bottom);
        ov += ov;
        ov >= b.height().min(self.height())
    }

    /// Computed in `int16_t` like the original.
    pub fn major_x_overlap(&self, b: &TBox) -> bool {
        let mut ov = b.width() as i16;
        if self.left > b.left {
            ov = ov.wrapping_sub((self.left - b.left) as i16);
        }
        if self.right < b.right {
            ov = ov.wrapping_sub((b.right - self.right) as i16);
        }
        i32::from(ov) >= b.width() / 2 || i32::from(ov) >= self.width() / 2
    }

    pub fn major_y_overlap(&self, b: &TBox) -> bool {
        let mut ov = b.height() as i16;
        if self.bottom > b.bottom {
            ov = ov.wrapping_sub((self.bottom - b.bottom) as i16);
        }
        if self.top < b.top {
            ov = ov.wrapping_sub((b.top - self.top) as i16);
        }
        i32::from(ov) >= b.height() / 2 || i32::from(ov) >= self.height() / 2
    }

    pub fn intersection(&self, b: &TBox) -> TBox {
        if self.overlap(b) {
            TBox::new(
                self.left.max(b.left),
                self.bottom.max(b.bottom),
                self.right.min(b.right),
                self.top.min(b.top),
            )
        } else {
            TBox::new(NULL_LO, NULL_LO, -NULL_LO, -NULL_LO)
        }
    }

    pub fn overlap_fraction(&self, b: &TBox) -> f64 {
        let a = self.area();
        if a != 0 {
            f64::from(self.intersection(b).area()) / f64::from(a)
        } else {
            0.0
        }
    }

    pub fn x_overlap_fraction(&self, o: &TBox) -> f64 {
        let low = self.left.max(o.left);
        let high = self.right.min(o.right);
        let width = self.right - self.left;
        if width == 0 {
            let x = self.left;
            if o.left <= x && x <= o.right {
                1.0
            } else {
                0.0
            }
        } else {
            (f64::from(high - low) / f64::from(width)).max(0.0)
        }
    }

    pub fn y_overlap_fraction(&self, o: &TBox) -> f64 {
        let low = self.bottom.max(o.bottom);
        let high = self.top.min(o.top);
        let height = self.top - self.bottom;
        if height == 0 {
            let y = self.bottom;
            if o.bottom <= y && y <= o.top {
                1.0
            } else {
                0.0
            }
        } else {
            (f64::from(high - low) / f64::from(height)).max(0.0)
        }
    }

    pub fn x_almost_equal(&self, b: &TBox, tol: i32) -> bool {
        (self.left - b.left).abs() <= tol && (self.right - b.right).abs() <= tol
    }

    pub fn almost_equal(&self, b: &TBox, tol: i32) -> bool {
        self.x_almost_equal(b, tol)
            && (self.top - b.top).abs() <= tol
            && (self.bottom - b.bottom).abs() <= tol
    }

    pub fn pad(&mut self, xpad: i32, ypad: i32) {
        self.left -= xpad;
        self.bottom -= ypad;
        self.right += xpad;
        self.top += ypad;
    }

    pub fn move_by(&mut self, v: ICoord) {
        self.left += v.x;
        self.right += v.x;
        self.bottom += v.y;
        self.top += v.y;
    }
}
