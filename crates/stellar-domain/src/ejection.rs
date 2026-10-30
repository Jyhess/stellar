//! Cleanup of ejected bodies, preserving bound clusters.
//!
//! A distant body (beyond k × R from the barycenter) that is not bound to
//! the system is removed, **unless** it belongs to a significant ejected
//! cluster:
//!
//! 1. distant bodies are grouped by "friends of friends" (two bodies closer
//!    than the link length belong to the same group);
//! 2. each group is pruned: members whose energy, measured in the group's
//!    frame and under the group's own potential, is positive are removed
//!    (most energetic first), until nothing changes;
//! 3. a bound group of at least `min_bodies` bodies, or with a mass of at
//!    least `min_mass`, is kept whole: it is an ejected cluster.
//!
//! Engines provide the distant bodies (with their potential in the whole
//! system); the rule picks those to remove and describes the clusters.

use serde::{Deserialize, Serialize};

use crate::{BodyId, BodySet, PhysicsParams, Vec3};

/// Beyond this size, the group's own potential is approximated by a
/// spherically symmetric distribution (O(n log n)) instead of being summed
/// exactly (O(n²)).
const EXACT_LIMIT: usize = 2048;

/// Distant body subject to cleanup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub id: BodyId,
    pub position: Vec3,
    pub velocity: Vec3,
    pub mass: f64,
    /// Gravitational potential felt in the whole system (per unit mass, G
    /// included).
    pub potential: f64,
}

/// Thresholds of the rule, taken from the physical parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EjectionRules {
    pub g: f64,
    pub softening: f64,
    pub link_length: f64,
    pub min_bodies: u32,
    /// Minimum mass of a cluster (absolute).
    pub min_mass: f64,
}

impl EjectionRules {
    /// `initial_mass`: total initial mass, to which the minimum share
    /// `cluster_min_mass` refers.
    pub fn new(params: &PhysicsParams, initial_mass: f64) -> Self {
        Self {
            g: params.g,
            softening: params.softening,
            link_length: params.cluster_link_length,
            min_bodies: params.cluster_min_bodies,
            min_mass: params.cluster_min_mass * initial_mass,
        }
    }

    /// Is a bound cluster of this size and mass kept?
    fn significant(&self, bodies: usize, mass: f64) -> bool {
        (self.min_bodies > 0 && bodies >= self.min_bodies as usize)
            || (self.min_mass > 0.0 && mass >= self.min_mass)
    }
}

/// Ejected cluster kept by the cleanup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EjectedCluster {
    /// Most massive body of the cluster (marker for tracking).
    pub anchor: BodyId,
    pub bodies: u32,
    pub mass: f64,
    pub center: Vec3,
    pub velocity: Vec3,
    /// Radius containing half of the mass, around the center.
    pub half_mass_radius: f64,
    /// Identifiers of the members.
    pub members: Vec<BodyId>,
}

/// Result of the cleanup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ejection {
    /// Indices (in the candidate list) of the bodies to remove, increasing.
    pub removed: Vec<usize>,
    /// Kept clusters, from most to least massive.
    pub clusters: Vec<EjectedCluster>,
}

/// Applies the rule to the distant bodies. `velocity`: velocity of the
/// barycenter of the whole system (frame of the "unbound" criterion).
pub fn sort_out(candidates: &[Candidate], velocity: Vec3, rules: &EjectionRules) -> Ejection {
    let mut keep = vec![false; candidates.len()];
    let mut clusters = Vec::new();
    for group in friends_of_friends(candidates, rules.link_length) {
        let bound = unbind(candidates, group, rules);
        let mass: f64 = bound.iter().map(|&i| candidates[i].mass).sum();
        if bound.is_empty() || !rules.significant(bound.len(), mass) {
            continue;
        }
        clusters.push(describe(candidates, &bound, mass));
        for &i in &bound {
            keep[i] = true;
        }
    }
    clusters.sort_by(|a, b| b.mass.total_cmp(&a.mass).then(a.anchor.cmp(&b.anchor)));
    let removed = candidates
        .iter()
        .enumerate()
        .filter(|&(i, c)| {
            !keep[i] && 0.5 * (c.velocity - velocity).length_squared() + c.potential > 0.0
        })
        .map(|(i, _)| i)
        .collect();
    Ejection { removed, clusters }
}

/// "Friends of friends" groups: connected components of the graph linking
/// bodies closer than `link`. Grid of cell size `link`, sorted cells.
pub fn friends_of_friends(candidates: &[Candidate], link: f64) -> Vec<Vec<usize>> {
    let n = candidates.len();
    if n == 0 {
        return Vec::new();
    }
    if link.is_nan() || link <= 0.0 {
        return (0..n).map(|i| vec![i]).collect();
    }
    let cell = |p: Vec3| -> [i64; 3] {
        let c = (p / link).floor();
        [c.x as i64, c.y as i64, c.z as i64]
    };
    let mut order: Vec<([i64; 3], usize)> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (cell(c.position), i))
        .collect();
    order.sort_unstable();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let link2 = link * link;
    for &(key, i) in &order {
        let pi = candidates[i].position;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let k = [key[0] + dx, key[1] + dy, key[2] + dz];
                    // Each pair of cells is examined only once.
                    if k < key {
                        continue;
                    }
                    let start = order.partition_point(|e| e.0 < k);
                    for &(kj, j) in order[start..].iter().take_while(|e| e.0 == k) {
                        if kj == key && j <= i {
                            continue;
                        }
                        if (candidates[j].position - pi).length_squared() <= link2 {
                            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                            if a != b {
                                parent[a.max(b)] = a.min(b);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut slot = vec![usize::MAX; n];
    for i in 0..n {
        let r = find(&mut parent, i);
        if slot[r] == usize::MAX {
            slot[r] = groups.len();
            groups.push(Vec::new());
        }
        groups[slot[r]].push(i);
    }
    groups
}

/// Pruning of a group: removes unbound members (positive energy within the
/// group) until nothing changes; returns the bound members.
fn unbind(candidates: &[Candidate], mut members: Vec<usize>, rules: &EjectionRules) -> Vec<usize> {
    if members.len() == 1 {
        // A lone body is "bound" to itself: only its mass matters.
        return members;
    }
    let eps2 = rules.softening * rules.softening;
    loop {
        if members.len() < 2 {
            return Vec::new();
        }
        let (mass, center, velocity) = barycenter(candidates, &members);
        if mass <= 0.0 {
            return Vec::new();
        }
        let potentials = if members.len() <= EXACT_LIMIT {
            exact_potentials(candidates, &members, rules.g, eps2)
        } else {
            spherical_potentials(candidates, &members, center, rules.g, eps2)
        };
        let energy: Vec<f64> = members
            .iter()
            .zip(&potentials)
            .map(|(&i, &phi)| 0.5 * (candidates[i].velocity - velocity).length_squared() + phi)
            .collect();
        let worst = energy.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if worst < 0.0 {
            return members;
        }
        // Only the most energetic leave at each round (at least half the energy
        // of the worst): a very fast intruder skews the group's velocity, and
        // removing at once every member that looks unbound could dissolve a
        // genuine cluster.
        let gone: Vec<bool> = energy.iter().map(|&e| e >= 0.5 * worst).collect();
        let mut k = 0;
        members.retain(|_| {
            k += 1;
            !gone[k - 1]
        });
    }
}

fn barycenter(candidates: &[Candidate], members: &[usize]) -> (f64, Vec3, Vec3) {
    let (mut m, mut x, mut v) = (0.0, Vec3::ZERO, Vec3::ZERO);
    for &i in members {
        let c = &candidates[i];
        m += c.mass;
        x += c.position * c.mass;
        v += c.velocity * c.mass;
    }
    if m > 0.0 {
        (m, x / m, v / m)
    } else {
        (m, x, v)
    }
}

/// Group's own potential, summed exactly (Plummer softening).
fn exact_potentials(candidates: &[Candidate], members: &[usize], g: f64, eps2: f64) -> Vec<f64> {
    let mut phi = vec![0.0; members.len()];
    for a in 0..members.len() {
        let ca = &candidates[members[a]];
        for b in a + 1..members.len() {
            let cb = &candidates[members[b]];
            let inv = 1.0 / ((ca.position - cb.position).length_squared() + eps2).sqrt();
            phi[a] -= g * cb.mass * inv;
            phi[b] -= g * ca.mass * inv;
        }
    }
    phi
}

/// Group's own potential, approximated by the spherical distribution with
/// the same radial profile around the barycenter:
/// φ(r) = −G (M(<r) / r + Σ_{rⱼ > r} mⱼ / rⱼ).
fn spherical_potentials(
    candidates: &[Candidate],
    members: &[usize],
    center: Vec3,
    g: f64,
    eps2: f64,
) -> Vec<f64> {
    let soft = |r: f64| (r * r + eps2).sqrt();
    let radius: Vec<f64> = members
        .iter()
        .map(|&i| (candidates[i].position - center).length())
        .collect();
    let mut order: Vec<usize> = (0..members.len()).collect();
    order.sort_unstable_by(|&a, &b| radius[a].total_cmp(&radius[b]));
    let mut phi = vec![0.0; members.len()];
    // Outer contribution: Σ mⱼ / rⱼ of the farther bodies.
    let mut outer = 0.0;
    for &k in order.iter().rev() {
        phi[k] = -g * outer;
        outer += candidates[members[k]].mass / soft(radius[k]);
    }
    // Inner contribution: M(<r) / r.
    let mut inner = 0.0;
    for &k in &order {
        phi[k] -= g * inner / soft(radius[k]);
        inner += candidates[members[k]].mass;
    }
    phi
}

fn describe(candidates: &[Candidate], members: &[usize], mass: f64) -> EjectedCluster {
    let (_, center, velocity) = barycenter(candidates, members);
    let anchor = members
        .iter()
        .map(|&i| &candidates[i])
        .max_by(|a, b| a.mass.total_cmp(&b.mass).then(b.id.cmp(&a.id)))
        .map(|c| c.id)
        .unwrap_or_default();
    let mut shells: Vec<(f64, f64)> = members
        .iter()
        .map(|&i| {
            let c = &candidates[i];
            ((c.position - center).length(), c.mass)
        })
        .collect();
    shells.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut acc = 0.0;
    let mut half_mass_radius = 0.0;
    for (r, m) in shells {
        acc += m;
        half_mass_radius = r;
        if acc >= 0.5 * mass {
            break;
        }
    }
    EjectedCluster {
        anchor,
        bodies: members.len() as u32,
        mass,
        center,
        velocity,
        half_mass_radius,
        members: members.iter().map(|&i| candidates[i].id).collect(),
    }
}

/// Search radius of the local cluster, in link lengths.
const LOCAL_SEARCH: f64 = 30.0;
/// Maximum number of bodies examined for the local cluster (the nearest).
const LOCAL_CAP: usize = 200_000;

/// Bound cluster containing the body of index `around`: neighbouring bodies
/// (within `LOCAL_SEARCH` link lengths), the "friends of friends" group
/// that contains it, pruned of its unbound members (group's own potential).
/// Returns the indices of the members; the body alone if it is bound to no
/// group.
pub fn local_cluster(bodies: &BodySet, around: usize, rules: &EjectionRules) -> Vec<usize> {
    let center = bodies.positions[around];
    let reach = LOCAL_SEARCH * rules.link_length;
    let mut near: Vec<(f64, usize)> = bodies
        .positions
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let d2 = (*p - center).length_squared();
            (d2 <= reach * reach).then_some((d2, i))
        })
        .collect();
    if near.len() > LOCAL_CAP {
        near.select_nth_unstable_by(LOCAL_CAP, |a, b| a.0.total_cmp(&b.0));
        near.truncate(LOCAL_CAP);
    }
    let indices: Vec<usize> = near.iter().map(|&(_, i)| i).collect();
    let candidates: Vec<Candidate> = indices
        .iter()
        .map(|&i| Candidate {
            id: bodies.ids[i],
            position: bodies.positions[i],
            velocity: bodies.velocities[i],
            mass: bodies.masses[i],
            potential: 0.0,
        })
        .collect();
    let Some(me) = indices.iter().position(|&i| i == around) else {
        return vec![around];
    };
    let Some(group) = friends_of_friends(&candidates, rules.link_length)
        .into_iter()
        .find(|g| g.contains(&me))
    else {
        return vec![around];
    };
    let bound = unbind(&candidates, group, rules);
    if bound.contains(&me) {
        bound.into_iter().map(|k| indices[k]).collect()
    } else {
        vec![around]
    }
}

