//! Détection des contacts « balayés » sur un pas.
//!
//! Chaque corps décrit un segment (position de début → position de fin) ;
//! deux corps sont en contact si la distance minimale entre eux au cours du
//! pas est inférieure à la somme de leurs rayons.
//!
//! - Phase large : grille hiérarchique sur les boîtes englobantes balayées
//!   (voir [`find_contacts`]).
//! - Phase fine : distance minimale exacte entre trajectoires rectilignes.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use glam::DVec3;
use rayon::prelude::*;
use stellar_domain::physics::swept_min_distance;

const AXIS_BITS: u32 = 21;
const AXIS_LIMIT: i64 = 1 << AXIS_BITS;

/// Hachage minimal pour des clés entières déjà bien réparties.
#[derive(Default)]
struct IdentityMix(u64);
impl Hasher for IdentityMix {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("clé u64 uniquement")
    }
    fn write_u64(&mut self, v: u64) {
        // Mélange multiplicatif (Fibonacci) pour disperser les clés voisines.
        self.0 = v.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
    }
}
type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<IdentityMix>>;

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

/// Décalages « vers l'avant » : chaque paire de cellules voisines n'est
/// examinée qu'une seule fois.
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

/// Rapport entre les tailles de cellule de deux niveaux consécutifs.
const LEVEL_RATIO: f64 = 4.0;
/// Au-delà, les corps (démesurément étendus) sont testés contre tous les autres.
const MAX_LEVEL: u32 = 12;

/// Grille uniforme : entrées (clé de cellule, indice) triées par clé.
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

    /// Membres d'une cellule (recherche dichotomique).
    fn cell_members(&self, key: u64) -> &[(u64, u32)] {
        let s = self.entries.partition_point(|e| e.0 < key);
        let e = s + self.entries[s..].partition_point(|e| e.0 == key);
        &self.entries[s..e]
    }

    /// Paires entre membres de la grille (cellule et 13 voisines « en avant »).
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
        let mut index: FastMap<u64, u32> = FastMap::default();
        index.reserve(cells.len());
        for (ci, c) in cells.iter().enumerate() {
            index.insert(c.0, ci as u32);
        }
        let offsets = forward_offsets();
        cells
            .par_iter()
            .flat_map_iter(|&(key, s, e)| {
                let mut local = Vec::new();
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
                for off in &offsets {
                    let nc = [c[0] + off[0], c[1] + off[1], c[2] + off[2]];
                    if !in_range(nc) {
                        continue;
                    }
                    if let Some(&ci) = index.get(&pack(nc)) {
                        let (_, ns, ne) = cells[ci as usize];
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

/// Renvoie les paires (i, j), i ≠ j, de corps entrés en contact durant le pas.
///
/// Grille hiérarchique : un corps de demi-extension e est rangé au plus petit
/// niveau L tel que e ≤ h_L / 2 (h_L = h₀·4^L, h₀ = 2 × 99ᵉ centile). Deux
/// corps d'un même niveau en contact occupent des cellules voisines ; un
/// corps de niveau L est confronté aux corps des niveaux inférieurs via les
/// 27 cellules voisines d'une grille de pas h_L. Les niveaux supérieurs sont
/// peu peuplés : leur coût reste marginal.
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
    let p99 = {
        let mut e = ext.clone();
        let k = (((n as f64) * 0.99) as usize).min(n - 1);
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

    // Niveau de chaque corps ; u32::MAX = démesuré.
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
    let mut lower: Vec<u32> = Vec::new(); // corps des niveaux déjà traités
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

    // Corps démesurés : test contre tous les autres.
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

/// Union-find avec compression de chemin, pour regrouper les fusions en
/// chaîne (A touche B qui touche C ⇒ un seul corps).
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
        // Quelques corps très rapides pour solliciter la voie « étendus ».
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
