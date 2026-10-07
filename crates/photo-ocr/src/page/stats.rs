//! Tesseract's `STATS` histogram.

#[derive(Clone, Debug, Default)]
pub struct Stats {
    rangemin: i32,
    rangemax: i32,
    total: i32,
    /// Empty for a default-constructed (bucketless) `STATS`.
    buckets: Vec<i32>,
}

impl Stats {
    /// `STATS(min, max)`; `max` is inclusive.
    pub fn new(min: i32, max: i32) -> Stats {
        let (min, max) = if max < min { (0, 1) } else { (min, max) };
        Stats {
            rangemin: min,
            rangemax: max,
            total: 0,
            buckets: vec![0; (1 + max - min) as usize],
        }
    }

    pub fn set_range(&mut self, min: i32, max: i32) -> bool {
        if max < min {
            return false;
        }
        *self = Stats::new(min, max);
        true
    }

    pub fn clear(&mut self) {
        self.total = 0;
        self.buckets.iter_mut().for_each(|b| *b = 0);
    }

    pub fn add(&mut self, value: i32, count: i32) {
        if self.buckets.is_empty() {
            return;
        }
        let v = value.clamp(self.rangemin, self.rangemax);
        self.buckets[(v - self.rangemin) as usize] += count;
        self.total += count;
    }

    pub fn get_total(&self) -> i32 {
        self.total
    }

    pub fn pile_count(&self, value: i32) -> i32 {
        if self.buckets.is_empty() {
            return 0;
        }
        if value <= self.rangemin {
            return self.buckets[0];
        }
        if value >= self.rangemax {
            return self.buckets[(self.rangemax - self.rangemin) as usize];
        }
        self.buckets[(value - self.rangemin) as usize]
    }

    pub fn mode(&self) -> i32 {
        if self.buckets.is_empty() {
            return self.rangemin;
        }
        let mut max = self.buckets[0];
        let mut maxindex = 0;
        for index in (1..=(self.rangemax - self.rangemin) as usize).rev() {
            if self.buckets[index] > max {
                max = self.buckets[index];
                maxindex = index as i32;
            }
        }
        maxindex + self.rangemin
    }

    pub fn mean(&self) -> f64 {
        if self.buckets.is_empty() || self.total <= 0 {
            return f64::from(self.rangemin);
        }
        let mut sum: i64 = 0;
        for (i, &b) in self.buckets.iter().enumerate().rev() {
            sum += i as i64 * i64::from(b);
        }
        sum as f64 / f64::from(self.total) + f64::from(self.rangemin)
    }

    pub fn sd(&self) -> f64 {
        if self.buckets.is_empty() || self.total <= 0 {
            return 0.0;
        }
        let mut sum: i64 = 0;
        let mut sqsum = 0.0f64;
        for (i, &b) in self.buckets.iter().enumerate().rev() {
            sum += i as i64 * i64::from(b);
            sqsum += i as f64 * i as f64 * f64::from(b);
        }
        let mut variance = sum as f64 / f64::from(self.total);
        variance = sqsum / f64::from(self.total) - variance * variance;
        if variance > 0.0 { variance.sqrt() } else { 0.0 }
    }

    pub fn ile(&self, frac: f64) -> f64 {
        if self.buckets.is_empty() || self.total == 0 {
            return f64::from(self.rangemin);
        }
        let target = (frac * f64::from(self.total)).clamp(1.0, f64::from(self.total));
        let mut sum: i32 = 0;
        let mut index = 0usize;
        let n = self.buckets.len();
        while index < n && f64::from(sum) < target {
            sum += self.buckets[index];
            index += 1;
        }
        if index > 0 {
            f64::from(self.rangemin) + index as f64
                - (f64::from(sum) - target) / f64::from(self.buckets[index - 1])
        } else {
            f64::from(self.rangemin)
        }
    }

    pub fn min_bucket(&self) -> i32 {
        if self.buckets.is_empty() || self.total == 0 {
            return self.rangemin;
        }
        let min = self
            .buckets
            .iter()
            .position(|&b| b != 0)
            .unwrap_or(self.buckets.len());
        self.rangemin + min as i32
    }

    pub fn max_bucket(&self) -> i32 {
        if self.buckets.is_empty() || self.total == 0 {
            return self.rangemin;
        }
        let mut max = self.buckets.len() - 1;
        while max > 0 && self.buckets[max] == 0 {
            max -= 1;
        }
        self.rangemin + max as i32
    }

    pub fn median(&self) -> f64 {
        if self.buckets.is_empty() {
            return f64::from(self.rangemin);
        }
        let mut median = self.ile(0.5);
        let median_pile = median.floor() as i32;
        if self.total > 1 && self.pile_count(median_pile) == 0 {
            let mut min_pile = median_pile;
            while self.pile_count(min_pile) == 0 {
                min_pile -= 1;
            }
            let mut max_pile = median_pile;
            while self.pile_count(max_pile) == 0 {
                max_pile += 1;
            }
            median = f64::from(min_pile + max_pile) / 2.0;
        }
        median
    }

    pub fn local_min(&self, x: i32) -> bool {
        if self.buckets.is_empty() {
            return false;
        }
        let x = (x.clamp(self.rangemin, self.rangemax) - self.rangemin) as isize;
        let b = &self.buckets;
        let bx = b[x as usize];
        if bx == 0 {
            return true;
        }
        let mut index = x - 1;
        while index >= 0 && b[index as usize] == bx {
            index -= 1;
        }
        if index >= 0 && b[index as usize] < bx {
            return false;
        }
        let top = (self.rangemax - self.rangemin) as isize;
        let mut index = x + 1;
        while index <= top && b[index as usize] == bx {
            index += 1;
        }
        !(index <= top && b[index as usize] < bx)
    }

    pub fn smooth(&mut self, factor: i32) {
        if self.buckets.is_empty() || factor < 2 {
            return;
        }
        let n = self.buckets.len() as i32;
        let mut result = Stats::new(self.rangemin, self.rangemax);
        for entry in 0..n {
            let mut count = self.buckets[entry as usize] * factor;
            for offset in 1..factor {
                if entry - offset >= 0 {
                    count += self.buckets[(entry - offset) as usize] * (factor - offset);
                }
                if entry + offset < n {
                    count += self.buckets[(entry + offset) as usize] * (factor - offset);
                }
            }
            result.add(entry + self.rangemin, count);
        }
        self.total = result.total;
        self.buckets = result.buckets;
    }

    /// `STATS::cluster`. `clusters` must hold `max_clusters + 1` entries.
    pub fn cluster(
        &self,
        lower: f32,
        upper: f32,
        multiple: f32,
        max_clusters: i32,
        clusters: &mut [Stats],
    ) -> i32 {
        if self.buckets.is_empty() || max_clusters < 1 {
            return 0;
        }
        let mut centres = vec![0f32; max_clusters as usize + 1];
        let mut new_centre = 0;
        let mut cluster_count = 1i32;
        while cluster_count <= max_clusters
            && !clusters[cluster_count as usize].buckets.is_empty()
            && clusters[cluster_count as usize].total > 0
        {
            let c = cluster_count as usize;
            centres[c] = clusters[c].ile(0.5) as f32;
            new_centre = clusters[c].mode();
            self.grow_cluster(c, new_centre, centres[c], lower, clusters);
            cluster_count += 1;
        }
        cluster_count -= 1;
        if cluster_count == 0 {
            clusters[0].set_range(self.rangemin, self.rangemax);
        }
        loop {
            let mut new_cluster = false;
            let mut new_mode = 0;
            for entry in 0..self.buckets.len() {
                let count = self.buckets[entry] - clusters[0].buckets[entry];
                if count > 0 {
                    let mut min_dist = i32::MAX as f32;
                    let mut best = 0usize;
                    let v = entry as i32 + self.rangemin;
                    for c in 1..=cluster_count as usize {
                        let dist = (v as f32 - centres[c]).abs();
                        if dist < min_dist {
                            min_dist = dist;
                            best = c;
                        }
                    }
                    if min_dist > upper
                        && (best == 0
                            || v as f32 > centres[best] * multiple
                            || (v as f32) < centres[best] / multiple)
                        && count > new_mode
                    {
                        new_mode = count;
                        new_centre = v;
                    }
                }
            }
            if new_mode > 0 && cluster_count < max_clusters {
                cluster_count += 1;
                new_cluster = true;
                let c = cluster_count as usize;
                if !clusters[c].set_range(self.rangemin, self.rangemax) {
                    return 0;
                }
                centres[c] = new_centre as f32;
                clusters[c].add(new_centre, new_mode);
                clusters[0].add(new_centre, new_mode);
                self.grow_cluster(c, new_centre, centres[c], lower, clusters);
                centres[c] = clusters[c].ile(0.5) as f32;
            }
            if !(new_cluster && cluster_count < max_clusters) {
                break;
            }
        }
        cluster_count
    }

    fn grow_cluster(&self, c: usize, centre: i32, cf: f32, lower: f32, clusters: &mut [Stats]) {
        let mut entry = centre - 1;
        while cf - (entry as f32) < lower
            && entry >= self.rangemin
            && self.pile_count(entry) <= self.pile_count(entry + 1)
        {
            let count = self.pile_count(entry) - clusters[0].pile_count(entry);
            if count > 0 {
                clusters[c].add(entry, count);
                clusters[0].add(entry, count);
            }
            entry -= 1;
        }
        let mut entry = centre + 1;
        while (entry as f32) - cf < lower
            && entry <= self.rangemax
            && self.pile_count(entry) <= self.pile_count(entry - 1)
        {
            let count = self.pile_count(entry) - clusters[0].pile_count(entry);
            if count > 0 {
                clusters[c].add(entry, count);
                clusters[0].add(entry, count);
            }
            entry += 1;
        }
    }

    /// `STATS::top_n_modes`: (mean, count) pairs, largest count first.
    pub fn top_n_modes(&self, max_modes: usize) -> Vec<(f32, i32)> {
        let mut modes: Vec<(f32, i32)> = Vec::new();
        if max_modes == 0 {
            return modes;
        }
        let n = self.buckets.len();
        let mut used = vec![0i32; n];
        let mut least_count = 1;
        loop {
            let mut max_count = 0;
            let mut max_index = 0usize;
            for i in 0..n {
                let pile = self.buckets[i] - used[i];
                if pile > max_count {
                    max_count = pile;
                    max_index = i;
                }
            }
            if max_count == 0 {
                break;
            }
            used[max_index] = max_count;
            let mut total_value = (max_index as i32 * max_count) as f64;
            let mut total_count = max_count;
            let mut prev = max_count;
            let mut gather = |i: usize, prev: &mut i32, tc: &mut i32, tv: &mut f64| {
                let pile = self.buckets[i] - used[i];
                if pile <= *prev && pile > 0 {
                    *tc += pile;
                    *tv += (i as i32 * pile) as f64;
                    used[i] = self.buckets[i];
                    *prev = pile;
                    true
                } else {
                    false
                }
            };
            let mut off = 1;
            while max_index + off < n {
                if !gather(
                    max_index + off,
                    &mut prev,
                    &mut total_count,
                    &mut total_value,
                ) {
                    break;
                }
                off += 1;
            }
            prev = self.buckets[max_index];
            let mut off = 1;
            while off <= max_index {
                if !gather(
                    max_index - off,
                    &mut prev,
                    &mut total_count,
                    &mut total_value,
                ) {
                    break;
                }
                off += 1;
            }
            if total_count > least_count || modes.len() < max_modes {
                if modes.len() == max_modes {
                    modes.truncate(max_modes - 1);
                }
                let mut t = 0;
                while t < modes.len() && modes[t].1 >= total_count {
                    t += 1;
                }
                let mean = (total_value / f64::from(total_count) + f64::from(self.rangemin)) as f32;
                modes.insert(t, (mean, total_count));
                least_count = modes.last().map_or(0, |m| m.1);
            }
        }
        modes
    }
}
