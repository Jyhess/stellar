//! Detection of "swept" contacts over a step.
//!
//! Each body describes a segment (start position → end position); two
//! bodies are in contact if their minimum distance during the step is less
//! than the sum of their radii.
//!
//! - Broad phase: hierarchical grid over the swept bounding boxes (see
//!   [`find_contacts`]).
//! - Narrow phase: exact minimum distance between straight trajectories.

use glam::DVec3;
use rayon::prelude::*;
use stellar_domain::physics::swept_min_distance;

const AXIS_BITS: u32 = 21;
const AXIS_LIMIT: i64 = 1 << AXIS_BITS;

#[derive(Clone, Copy)]
struct Swept {
    lo: DVec3,
    hi: DVec3,
}

impl Swept {
    #[inline]
    fn overlaps(&self, o: &Swept) -> bool {
        self.lo.x <= o.hi.x
            && o.lo.x <= self.hi.x
            && self.lo.y <= o.hi.y
            && o.lo.y <= self.hi.y
            && self.lo.z <= o.hi.z
            && o.lo.z <= self.hi.z
    }
    #[inline]
    fn center(&self) -> DVec3 {
        (self.lo + self.hi) * 0.5
    }
    #[inline]
    fn extent(&self) -> f64 {
        ((self.hi - self.lo) * 0.5).max_element()
    }
}

#[inline]
fn pack(c: [i64; 3]) -> u64 {
    (c[0] as u64) | ((c[1] as u64) << AXIS_BITS) | ((c[2] as u64) << (2 * AXIS_BITS))
}

/// "Forward" offsets: each pair of neighbouring cells is examined only
/// once.
fn forward_offsets() -> Vec<[i64; 3]> {
    let mut v = Vec::with_capacity(13);
    for dz in -1..=1i64 {
        for dy in -1..=1i64 {
            for dx in -1..=1i64 {
                if (dz, dy, dx) > (0, 0, 0) {
                    v.push([dx, dy, dz]);
                }
            }
        }
    }
    v
}

/// Ratio between the cell sizes of two consecutive levels.
const LEVEL_RATIO: f64 = 4.0;
/// Beyond it, bodies (oversized) are tested against all the others.
const MAX_LEVEL: u32 = 12;

/// Uniform grid: entries (cell key, index) sorted by key.
struct Grid {
    cell: f64,
    origin: DVec3,
    entries: Vec<(u64, u32)>,
}

impl Grid {
    fn build(members: &[u32], boxes: &[Swept], origin: DVec3, cell: f64) -> Self {
        let mut entries: Vec<(u64, u32)> = members
            .par_iter()
            .map(|&i| (pack(coord(boxes[i as usize].center(), origin, cell)), i))
            .collect();
        entries.par_sort_unstable_by_key(|e| e.0);
        Self {
            cell,
            origin,
            entries,
        }
    }

    fn coord(&self, p: DVec3) -> [i64; 3] {
        coord(p, self.origin, self.cell)
    }

    /// Members of a cell (binary search).
    fn cell_members(&self, key: u64) -> &[(u64, u32)] {
        let s = self.entries.partition_point(|e| e.0 < key);
        let e = s + self.entries[s..].partition_point(|e| e.0 == key);
        &self.entries[s..e]
    }

    /// Pairs between members of the grid (cell and its 13 "forward" neighbours).
    ///
    /// A cell's key is linear in its coordinates: the neighbour at offset
    /// `off` has key `key + δ(off)`, and key order is preserved. For each
    /// offset, a cursor therefore advances along with the cells (merge of two
    /// sorted lists): no hash table, no search, only sequential accesses.
    fn self_pairs(&self, touches: &(impl Fn(usize, usize) -> bool + Sync)) -> Vec<(u32, u32)> {
        let entries = &self.entries;
        let mut cells: Vec<(u64, u32, u32)> = Vec::new();
        let mut s = 0usize;
        while s < entries.len() {
            let k = entries[s].0;
            let mut e = s + 1;
            while e < entries.len() && entries[e].0 == k {
                e += 1;
            }
            cells.push((k, s as u32, e as u32));
            s = e;
        }
        let offsets = forward_offsets();
        let deltas: Vec<i64> = offsets
            .iter()
            .map(|o| o[0] + (o[1] << AXIS_BITS) + (o[2] << (2 * AXIS_BITS)))
            .collect();
        const CHUNK: usize = 2048;
        cells
            .par_chunks(CHUNK)
            .flat_map_iter(|chunk| {
                let mut local = Vec::new();
                // Cursors: first cell whose key is ≥ the chunk's key + δ.
                let first = chunk[0].0 as i64;
                let mut cursor: Vec<usize> = deltas
                    .iter()
                    .map(|&d| cells.partition_point(|c| (c.0 as i64) < first + d))
                    .collect();
                for &(key, s, e) in chunk {
                    let members = &entries[s as usize..e as usize];
                    for a in 0..members.len() {
                        for b in a + 1..members.len() {
                            let (i, j) = (members[a].1 as usize, members[b].1 as usize);
                            if touches(i, j) {
                                local.push((i as u32, j as u32));
                            }
                        }
                    }
                    let c = unpack(key);
                    for (k, off) in offsets.iter().enumerate() {
                        let target = key as i64 + deltas[k];
                        let cur = &mut cursor[k];
                        while *cur < cells.len() && (cells[*cur].0 as i64) < target {
                            *cur += 1;
                        }
                        let nc = [c[0] + off[0], c[1] + off[1], c[2] + off[2]];
                        if *cur >= cells.len() || cells[*cur].0 as i64 != target || !in_range(nc) {
                            continue;
                        }
                        let (_, ns, ne) = cells[*cur];
                        for a in members {
                            for b in &entries[ns as usize..ne as usize] {
                                let (i, j) = (a.1 as usize, b.1 as usize);
                                if touches(i, j) {
                                    local.push((i as u32, j as u32));
                                }
                            }
                        }
                    }
                }
                local
            })
            .collect()
    }
}

#[inline]
fn coord(p: DVec3, origin: DVec3, cell: f64) -> [i64; 3] {
    let q = (p - origin) / cell;
    [q.x as i64, q.y as i64, q.z as i64]
}

#[inline]
fn unpack(key: u64) -> [i64; 3] {
    let mask = (1u64 << AXIS_BITS) - 1;
    [
        (key & mask) as i64,
        ((key >> AXIS_BITS) & mask) as i64,
        ((key >> (2 * AXIS_BITS)) & mask) as i64,
    ]
}

#[inline]
fn in_range(c: [i64; 3]) -> bool {
    c.iter().all(|&v| (0..AXIS_LIMIT).contains(&v))
}

/// Returns the pairs (i, j), i ≠ j, of bodies that came into contact during
/// the step.
///
/// Hierarchical grid: a body of half-extent e goes to the lowest level L
/// such that e ≤ h_L / 2 (h_L = h₀·4^L, h₀ = 2 × 99th percentile). Two
/// bodies of the same level in contact occupy neighbouring cells; a body of
/// level L is tested against the bodies of lower levels through the 27
/// neighbouring cells of a grid of cell size h_L. Upper levels are sparsely
/// populated: their cost stays marginal.
pub fn find_contacts(old: &[DVec3], new: &[DVec3], radius: &[f64]) -> Vec<(u32, u32)> {
    let n = new.len();
    if n < 2 {
        return Vec::new();
    }

    let boxes: Vec<Swept> = (0..n)
        .into_par_iter()
        .map(|i| {
            let r = DVec3::splat(radius[i]);
            Swept {
                lo: old[i].min(new[i]) - r,
                hi: old[i].max(new[i]) + r,
            }
        })
        .collect();
    let touches = |i: usize, j: usize| -> bool {
        boxes[i].overlaps(&boxes[j])
            && swept_min_distance(old[i], new[i], old[j], new[j]) <= radius[i] + radius[j]
    };

    let ext: Vec<f64> = boxes.par_iter().map(Swept::extent).collect();
    // 99th percentile of the extents, estimated on a regular sample: it only
    // sets the cell size, not the accuracy of the result.
    let p99 = {
        let stride = (n / 65_536).max(1);
        let mut e: Vec<f64> = ext.iter().step_by(stride).copied().collect();
        let k = (((e.len() as f64) * 0.99) as usize).min(e.len() - 1);
        *e.select_nth_unstable_by(k, f64::total_cmp).1
    };

    let (lo, hi) = boxes
        .par_iter()
        .map(|b| b.center())
        .fold(
            || (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
            |(a, b), c| (a.min(c), b.max(c)),
        )
        .reduce(
            || (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
            |a, b| (a.0.min(b.0), a.1.max(b.1)),
        );
    let span = (hi - lo).max_element();
    let h0 = (2.0 * p99).max(span / (AXIS_LIMIT - 2) as f64).max(1e-12);

    // Level of each body; u32::MAX = oversized.
    let level: Vec<u32> = ext
        .par_iter()
        .map(|&e| {
            let mut l = 0u32;
            let mut half = 0.5 * h0;
            while e > half {
                l += 1;
                half *= LEVEL_RATIO;
                if l > MAX_LEVEL {
                    return u32::MAX;
                }
            }
            l
        })
        .collect();
    let top = level
        .iter()
        .copied()
        .filter(|&l| l != u32::MAX)
        .max()
        .unwrap_or(0);

    let mut pairs: Vec<(u32, u32)> = Vec::new();
    let mut lower: Vec<u32> = Vec::new(); // bodies of the levels already processed
    for l in 0..=top {
        let members: Vec<u32> = (0..n as u32)
            .into_par_iter()
            .filter(|&i| level[i as usize] == l)
            .collect();
        if members.is_empty() {
            continue;
        }
        let cell = h0 * LEVEL_RATIO.powi(l as i32);
        let grid = Grid::build(&members, &boxes, lo, cell);
        pairs.extend(grid.self_pairs(&touches));

        if !lower.is_empty() {
            let low = Grid::build(&lower, &boxes, lo, cell);
            let cross: Vec<(u32, u32)> = members
                .par_iter()
                .flat_map_iter(|&j| {
                    let mut local = Vec::new();
                    let c = low.coord(boxes[j as usize].center());
                    for dz in -1..=1 {
                        for dy in -1..=1 {
                            for dx in -1..=1 {
                                let nc = [c[0] + dx, c[1] + dy, c[2] + dz];
                                if !in_range(nc) {
                                    continue;
                                }
                                for &(_, i) in low.cell_members(pack(nc)) {
                                    if touches(j as usize, i as usize) {
                                        local.push((j, i));
                                    }
                                }
                            }
                        }
                    }
                    local
                })
                .collect();
            pairs.extend(cross);
        }
        lower.extend_from_slice(&members);
    }

    // Oversized bodies: tested against all the others.
    let huge: Vec<usize> = (0..n).filter(|&i| level[i] == u32::MAX).collect();
    for &b in &huge {
        let found: Vec<(u32, u32)> = (0..n)
            .into_par_iter()
            .filter(|&j| j != b && !(level[j] == u32::MAX && j < b) && touches(b, j))
            .map(|j| (b as u32, j as u32))
            .collect();
        pairs.extend(found);
    }
    pairs
}

/// Union-find with path compression, to group chained mergers (A touches B,
/// which touches C ⇒ a single body).
pub struct DisjointSets {
    parent: Vec<u32>,
}

impl DisjointSets {
    pub fn new(n: usize) -> Self {
        Self {
            parent: (0..n as u32).collect(),
        }
    }

    pub fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let p = self.parent[x as usize];
            self.parent[x as usize] = self.parent[p as usize];
            x = p;
        }
        x
    }

    pub fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.parent[hi as usize] = lo;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute(old: &[DVec3], new: &[DVec3], r: &[f64]) -> Vec<(u32, u32)> {
        let mut v = Vec::new();
        for i in 0..new.len() {
            for j in i + 1..new.len() {
                if swept_min_distance(old[i], new[i], old[j], new[j]) <= r[i] + r[j] {
                    v.push((i as u32, j as u32));
                }
            }
        }
        v
    }

    fn normalise(mut v: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
        for p in &mut v {
            if p.0 > p.1 {
                *p = (p.1, p.0);
            }
        }
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn grid_matches_brute_force() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand_pcg::Pcg64Mcg::seed_from_u64(3);
        let n = 3_000;
        let old: Vec<DVec3> = (0..n)
            .map(|_| {
                DVec3::new(
                    rng.gen_range(-20.0..20.0),
                    rng.gen_range(-20.0..20.0),
                    rng.gen_range(-20.0..20.0),
                )
            })
            .collect();
        // A few very fast bodies to exercise the "extended" path.
        let new: Vec<DVec3> = old
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let speed = if i % 97 == 0 { 15.0 } else { 0.3 };
                *p + DVec3::new(
                    rng.gen_range(-1.0..1.0),
                    rng.gen_range(-1.0..1.0),
                    rng.gen_range(-1.0..1.0),
                ) * speed
            })
            .collect();
        let r: Vec<f64> = (0..n)
            .map(|i| if i % 211 == 0 { 2.0 } else { 0.25 })
            .collect();
        let fast = normalise(find_contacts(&old, &new, &r));
        let slow = normalise(brute(&old, &new, &r));
        assert!(!slow.is_empty());
        assert_eq!(fast, slow);
    }

    #[test]
    fn union_find_groups_chains() {
        let mut d = DisjointSets::new(5);
        d.union(0, 1);
        d.union(1, 2);
        d.union(3, 4);
        assert_eq!(d.find(2), d.find(0));
        assert_ne!(d.find(0), d.find(3));
    }
}
