//! `DetLineFit`: deterministic robust line fitting (`detlinefit.cpp`).

use super::geom::ICoord;
use super::stdalgo::nth_element;

const NUM_END_POINTS: usize = 3;
const MIN_POINTS_FOR_ERROR_COUNT: usize = 16;
const MAX_REAL_DISTANCE: f64 = 2.0;

pub fn int_cast_rounded(x: f64) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        -((-x + 0.5) as i32)
    }
}

pub fn int_cast_rounded_f32(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        -((-x + 0.5) as i32)
    }
}

#[derive(Clone, Copy, Debug)]
struct PointWidth {
    pt: ICoord,
    halfwidth: i32,
}

#[derive(Default, Debug)]
pub struct DetLineFit {
    pts: Vec<PointWidth>,
    distances: Vec<(f64, ICoord)>,
    square_length: f64,
}

impl DetLineFit {
    pub fn new() -> DetLineFit {
        DetLineFit::default()
    }

    pub fn clear(&mut self) {
        self.pts.clear();
        self.distances.clear();
    }

    pub fn add(&mut self, pt: ICoord) {
        self.pts.push(PointWidth { pt, halfwidth: 0 });
    }

    pub fn add_width(&mut self, pt: ICoord, halfwidth: i32) {
        self.pts.push(PointWidth { pt, halfwidth });
    }

    /// `Fit(skip_first, skip_last, pt1, pt2)`.
    pub fn fit_skip(&mut self, skip_first: usize, skip_last: usize) -> (f64, ICoord, ICoord) {
        if self.pts.is_empty() {
            return (0.0, ICoord::default(), ICoord::default());
        }
        let n = self.pts.len();
        let skip_first = skip_first.min(n - 1);
        let starts: Vec<ICoord> = (skip_first..(skip_first + NUM_END_POINTS).min(n))
            .map(|i| self.pts[i].pt)
            .collect();
        let skip_last = skip_last.min(n - 1);
        let end_i = n.saturating_sub(NUM_END_POINTS + skip_last);
        let mut ends = Vec::new();
        let mut i = (n - 1 - skip_last) as isize;
        while i >= end_i as isize {
            ends.push(self.pts[i as usize].pt);
            i -= 1;
        }
        if n <= 2 {
            let p1 = starts[0];
            let p2 = if n > 1 { ends[0] } else { p1 };
            return (0.0, p1, p2);
        }
        let mut best_uq = -1.0;
        let (mut p1, mut p2) = (ICoord::default(), ICoord::default());
        for &start in &starts {
            for &end in &ends {
                if start != end {
                    self.compute_distances(start, end);
                    let dist = self.evaluate_line_fit();
                    if dist < best_uq || best_uq < 0.0 {
                        best_uq = dist;
                        p1 = start;
                        p2 = end;
                    }
                }
            }
        }
        (
            if best_uq > 0.0 {
                best_uq.sqrt()
            } else {
                best_uq
            },
            p1,
            p2,
        )
    }

    pub fn fit(&mut self) -> (f64, ICoord, ICoord) {
        self.fit_skip(0, 0)
    }

    /// `Fit(float* m, float* c)`.
    pub fn fit_mc(&mut self) -> (f64, f32, f32) {
        let (err, start, end) = self.fit();
        if end.x != start.x {
            let m = (end.y - start.y) as f32 / (end.x - start.x) as f32;
            let c = start.y as f32 - m * start.x as f32;
            (err, m, c)
        } else {
            (err, 0.0, 0.0)
        }
    }

    /// `ConstrainedFit(direction, min_dist, max_dist, debug, line_pt)`.
    pub fn constrained_fit_dir(
        &mut self,
        dir: (f32, f32),
        min_dist: f64,
        max_dist: f64,
    ) -> (f64, ICoord) {
        self.compute_constrained_distances(dir, min_dist, max_dist);
        if self.pts.is_empty() || self.distances.is_empty() {
            return (0.0, ICoord::default());
        }
        let median = self.distances.len() / 2;
        nth_element(&mut self.distances, median, |a, b| a.0 < b.0);
        let line_pt = self.distances[median].1;
        let dist_origin = f64::from(dir.0 * line_pt.y as f32 - dir.1 * line_pt.x as f32);
        for d in &mut self.distances {
            d.0 -= dist_origin;
        }
        (self.evaluate_line_fit().sqrt(), line_pt)
    }

    /// `ConstrainedFit(double m, float* c)`.
    pub fn constrained_fit_m(&mut self, m: f64) -> (f64, f32) {
        if self.pts.is_empty() {
            return (0.0, 0.0);
        }
        let cos = 1.0 / (1.0 + m * m).sqrt();
        let dir = (cos as f32, (m * cos) as f32);
        let (err, pt) = self.constrained_fit_dir(dir, -f64::from(f32::MAX), f64::from(f32::MAX));
        let c = (f64::from(pt.y) - f64::from(pt.x) * m) as f32;
        (err, c)
    }

    pub fn sufficient_points_for_independent_fit(&self) -> bool {
        self.distances.len() >= MIN_POINTS_FOR_ERROR_COUNT
    }

    fn evaluate_line_fit(&mut self) -> f64 {
        let mut dist = self.compute_upper_quartile_error();
        if self.distances.len() >= MIN_POINTS_FOR_ERROR_COUNT
            && dist > MAX_REAL_DISTANCE * MAX_REAL_DISTANCE
        {
            let threshold = MAX_REAL_DISTANCE * self.square_length.sqrt();
            dist = self.distances.iter().filter(|d| d.0 > threshold).count() as f64;
        }
        dist
    }

    fn compute_upper_quartile_error(&mut self) -> f64 {
        let n = self.distances.len();
        if n == 0 {
            return 0.0;
        }
        for d in &mut self.distances {
            if d.0 < 0.0 {
                d.0 = -d.0;
            }
        }
        let index = 3 * n / 4;
        nth_element(&mut self.distances, index, |a, b| a.0 < b.0);
        let dist = self.distances[index].0;
        if self.square_length > 0.0 {
            dist * dist / self.square_length
        } else {
            0.0
        }
    }

    fn compute_distances(&mut self, start: ICoord, end: ICoord) {
        self.distances.clear();
        let line = end - start;
        self.square_length = f64::from(line.sqlength());
        let line_length = int_cast_rounded(self.square_length.sqrt());
        let mut prev_abs_dist = 0;
        let mut prev_dot = 0;
        for i in 0..self.pts.len() {
            let v = self.pts[i].pt - start;
            let dot = line.dot(v);
            let dist = line.cross(v);
            let abs_dist = dist.abs();
            if abs_dist > prev_abs_dist && i > 0 {
                let sep = (dot - prev_dot).abs();
                if sep < line_length * self.pts[i].halfwidth
                    || sep < line_length * self.pts[i - 1].halfwidth
                {
                    continue;
                }
            }
            self.distances.push((f64::from(dist), self.pts[i].pt));
            prev_abs_dist = abs_dist;
            prev_dot = dot;
        }
    }

    fn compute_constrained_distances(&mut self, dir: (f32, f32), min_dist: f64, max_dist: f64) {
        self.distances.clear();
        self.square_length = f64::from(dir.0 * dir.0 + dir.1 * dir.1);
        for p in &self.pts {
            let (px, py) = (p.pt.x as f32, p.pt.y as f32);
            let dist = f64::from(dir.0 * py - dir.1 * px);
            if min_dist <= dist && dist <= max_dist {
                self.distances.push((dist, p.pt));
            }
        }
    }
}
