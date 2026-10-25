//! Octree de Barnes-Hut, construit sur des corps triés par code de Morton.
//!
//! Les nœuds sont rangés en ordre préfixe (parcours en profondeur) : le
//! premier enfant d'un nœud interne le suit immédiatement, et `subtree`
//! (taille du sous-arbre) permet de sauter un sous-arbre entier. Le parcours
//! se fait donc sans pile ni pointeurs, de manière très favorable au cache.
//!
//! Le tri de Morton garantit que les corps d'un nœud sont contigus :
//! `[start, start + count)`.

use glam::DVec3;
use rayon::prelude::*;

use crate::morton::{Cube, BITS_PER_AXIS};

/// Nombre maximal de corps dans une feuille.
pub const LEAF_SIZE: usize = 16;
/// En dessous de ce nombre de corps, un sous-arbre est construit séquentiellement.
const PARALLEL_THRESHOLD: usize = 32_768;

#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// Centre de masse.
    pub com: DVec3,
    pub mass: f64,
    /// Carré du rayon critique : au-delà, le nœud est assimilé à un point.
    /// r_crit = côté/θ + |centre de masse − centre géométrique|.
    pub crit2: f64,
    pub start: u32,
    pub count: u32,
    /// Nombre de nœuds du sous-arbre, nœud courant compris (1 = feuille).
    pub subtree: u32,
}

impl Node {
    #[inline]
    pub fn is_leaf(&self) -> bool {
        self.subtree == 1
    }
}

struct Ctx<'a> {
    codes: &'a [u64],
    pos: &'a [DVec3],
    mass: &'a [f64],
    inv_theta: f64,
}

/// Construit l'octree. `codes` doivent être triés et correspondre à `pos`.
pub fn build(codes: &[u64], pos: &[DVec3], mass: &[f64], cube: Cube, theta: f64) -> Vec<Node> {
    let mut out = Vec::with_capacity(pos.len() / 4 + 16);
    if pos.is_empty() {
        return out;
    }
    let ctx = Ctx { codes, pos, mass, inv_theta: 1.0 / theta.max(1e-3) };
    build_node(&ctx, 0, pos.len(), 0, cube.min, cube.side, &mut out);
    out
}

/// Bornes des 8 enfants d'une plage triée au niveau `level`.
fn child_ranges(codes: &[u64], start: usize, end: usize, level: u32) -> [(usize, usize); 8] {
    let shift = 3 * (BITS_PER_AXIS - level - 1);
    let slice = &codes[start..end];
    let mut bounds = [0usize; 9];
    bounds[8] = slice.len();
    for c in 1..8u64 {
        bounds[c as usize] = slice.partition_point(|&code| ((code >> shift) & 7) < c);
    }
    let mut r = [(0, 0); 8];
    for c in 0..8 {
        r[c] = (start + bounds[c], start + bounds[c + 1]);
    }
    r
}

#[inline]
fn child_min(cmin: DVec3, half: f64, c: usize) -> DVec3 {
    cmin + DVec3::new(
        (c & 1) as f64 * half,
        ((c >> 1) & 1) as f64 * half,
        ((c >> 2) & 1) as f64 * half,
    )
}

fn make_node(ctx: &Ctx, com: DVec3, mass: f64, cmin: DVec3, side: f64, start: usize, end: usize, subtree: usize) -> Node {
    let center = cmin + DVec3::splat(0.5 * side);
    let r_crit = side * ctx.inv_theta + (com - center).length();
    Node {
        com,
        mass,
        crit2: r_crit * r_crit,
        start: start as u32,
        count: (end - start) as u32,
        subtree: subtree as u32,
    }
}

fn build_node(ctx: &Ctx, start: usize, end: usize, level: u32, cmin: DVec3, side: f64, out: &mut Vec<Node>) {
    let count = end - start;
    let me = out.len();

    if count <= LEAF_SIZE || level >= BITS_PER_AXIS {
        let mut m = 0.0;
        let mut mx = DVec3::ZERO;
        for j in start..end {
            m += ctx.mass[j];
            mx += ctx.pos[j] * ctx.mass[j];
        }
        let com = if m > 0.0 { mx / m } else { cmin + DVec3::splat(0.5 * side) };
        out.push(make_node(ctx, com, m, cmin, side, start, end, 1));
        return;
    }

    let ranges = child_ranges(ctx.codes, start, end, level);
    let half = 0.5 * side;
    // Emplacement réservé pour le nœud courant, complété après les enfants.
    out.push(Node { com: DVec3::ZERO, mass: 0.0, crit2: 0.0, start: 0, count: 0, subtree: 0 });

    let mut m = 0.0;
    let mut mx = DVec3::ZERO;

    if count >= PARALLEL_THRESHOLD {
        let subs: Vec<Vec<Node>> = ranges
            .par_iter()
            .enumerate()
            .filter(|(_, (a, b))| b > a)
            .map(|(c, &(a, b))| {
                let mut v = Vec::with_capacity((b - a) / 4 + 8);
                build_node(ctx, a, b, level + 1, child_min(cmin, half, c), half, &mut v);
                v
            })
            .collect();
        for sub in subs {
            m += sub[0].mass;
            mx += sub[0].com * sub[0].mass;
            out.extend(sub);
        }
    } else {
        for (c, &(a, b)) in ranges.iter().enumerate() {
            if b > a {
                let child = out.len();
                build_node(ctx, a, b, level + 1, child_min(cmin, half, c), half, out);
                m += out[child].mass;
                mx += out[child].com * out[child].mass;
            }
        }
    }

    let com = if m > 0.0 { mx / m } else { cmin + DVec3::splat(half) };
    let subtree = out.len() - me;
    out[me] = make_node(ctx, com, m, cmin, side, start, end, subtree);
}

/// Accélération (sans le facteur G) et potentiel (sans G) subis par le corps
/// `i` situé en `p`, avec adoucissement de Plummer ε² = `eps2`.
#[inline]
pub fn field_at(nodes: &[Node], pos: &[DVec3], mass: &[f64], i: usize, p: DVec3, eps2: f64) -> (DVec3, f64) {
    let mut acc = DVec3::ZERO;
    let mut phi = 0.0;
    let mut k = 0usize;
    let len = nodes.len();
    while k < len {
        let n = &nodes[k];
        let d = n.com - p;
        let r2 = d.length_squared();
        if r2 > n.crit2 {
            // Nœud lointain : approximation monopolaire.
            let inv = 1.0 / (r2 + eps2).sqrt();
            let inv3 = inv * inv * inv;
            acc += d * (n.mass * inv3);
            phi -= n.mass * inv;
            k += n.subtree as usize;
        } else if n.is_leaf() {
            // Feuille proche : somme directe.
            let s = n.start as usize;
            let e = s + n.count as usize;
            for j in s..e {
                if j == i {
                    continue;
                }
                let d = pos[j] - p;
                let r2 = d.length_squared() + eps2;
                if r2 == 0.0 {
                    continue;
                }
                let inv = 1.0 / r2.sqrt();
                let inv3 = inv * inv * inv;
                acc += d * (mass[j] * inv3);
                phi -= mass[j] * inv;
            }
            k += 1;
        } else {
            k += 1;
        }
    }
    (acc, phi)
}
