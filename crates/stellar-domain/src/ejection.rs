//! Nettoyage des corps éjectés, en préservant les amas liés.
//!
//! Un corps lointain (au-delà de k × R du barycentre) et non lié au système
//! est éliminé, **sauf** s'il appartient à un amas éjecté significatif :
//!
//! 1. les corps lointains sont regroupés par « amis d'amis » (deux corps à
//!    moins de la distance de liaison appartiennent au même groupe) ;
//! 2. chaque groupe est épuré : les membres dont l'énergie, mesurée dans le
//!    référentiel du groupe et sous son seul potentiel, est positive en sont
//!    retirés (les plus énergiques d'abord), jusqu'à stabilité ;
//! 3. un groupe lié d'au moins `min_bodies` corps, ou d'une masse au moins
//!    égale à `min_mass`, est conservé en entier : c'est un amas éjecté.
//!
//! Les moteurs fournissent les corps lointains (avec leur potentiel dans le
//! système entier) ; la règle désigne ceux à éliminer et décrit les amas.

use serde::{Deserialize, Serialize};

use crate::{BodyId, PhysicsParams, Vec3};

/// Au-delà de cette taille, le potentiel propre d'un groupe est approché par
/// une distribution à symétrie sphérique (O(n log n)) plutôt que sommé
/// exactement (O(n²)).
const EXACT_LIMIT: usize = 2048;

/// Corps lointain soumis au nettoyage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub id: BodyId,
    pub position: Vec3,
    pub velocity: Vec3,
    pub mass: f64,
    /// Potentiel gravitationnel ressenti dans le système entier (par unité
    /// de masse, G inclus).
    pub potential: f64,
}

/// Seuils de la règle, tirés des paramètres physiques.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EjectionRules {
    pub g: f64,
    pub softening: f64,
    pub link_length: f64,
    pub min_bodies: u32,
    /// Masse minimale d'un amas (absolue).
    pub min_mass: f64,
}

impl EjectionRules {
    /// `initial_mass` : masse totale initiale, à laquelle se rapporte la part
    /// minimale `cluster_min_mass`.
    pub fn new(params: &PhysicsParams, initial_mass: f64) -> Self {
        Self {
            g: params.g,
            softening: params.softening,
            link_length: params.cluster_link_length,
            min_bodies: params.cluster_min_bodies,
            min_mass: params.cluster_min_mass * initial_mass,
        }
    }

    /// Un amas lié de cette taille et de cette masse est-il conservé ?
    fn significant(&self, bodies: usize, mass: f64) -> bool {
        (self.min_bodies > 0 && bodies >= self.min_bodies as usize)
            || (self.min_mass > 0.0 && mass >= self.min_mass)
    }
}

/// Amas éjecté conservé par le nettoyage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EjectedCluster {
    /// Corps le plus massif de l'amas (repère pour le suivi).
    pub anchor: BodyId,
    pub bodies: u32,
    pub mass: f64,
    pub center: Vec3,
    pub velocity: Vec3,
    /// Rayon contenant la moitié de la masse, autour du centre.
    pub half_mass_radius: f64,
    /// Identifiants des membres.
    pub members: Vec<BodyId>,
}

/// Résultat du nettoyage.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ejection {
    /// Indices (dans la liste des candidats) des corps à éliminer, croissants.
    pub removed: Vec<usize>,
    /// Amas conservés, du plus massif au moins massif.
    pub clusters: Vec<EjectedCluster>,
}

/// Applique la règle aux corps lointains. `velocity` : vitesse du barycentre
/// du système entier (référentiel du critère « non lié »).
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

/// Groupes « amis d'amis » : composantes connexes du graphe reliant les corps
/// distants de moins de `link`. Grille de pas `link`, cellules triées.
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
                    // Chaque paire de cellules n'est examinée qu'une fois.
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

/// Épuration d'un groupe : retire les membres non liés (énergie propre au
/// groupe positive) jusqu'à stabilité ; renvoie les membres liés.
fn unbind(candidates: &[Candidate], mut members: Vec<usize>, rules: &EjectionRules) -> Vec<usize> {
    if members.len() == 1 {
        // Un corps isolé est « lié » à lui-même : seule sa masse compte.
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
        // Seuls les plus énergiques partent à chaque tour (au moins la moitié
        // de l'énergie du pire) : un intrus très rapide fausse la vitesse du
        // groupe, et retirer d'un coup tous les membres apparemment non liés
        // pourrait dissoudre un amas véritable.
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

/// Potentiel propre au groupe, sommé exactement (adoucissement de Plummer).
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

/// Potentiel propre au groupe, approché par la distribution sphérique de
/// même profil radial autour du barycentre :
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
    // Contribution extérieure : Σ mⱼ / rⱼ des corps plus éloignés.
    let mut outer = 0.0;
    for &k in order.iter().rev() {
        phi[k] = -g * outer;
        outer += candidates[members[k]].mass / soft(radius[k]);
    }
    // Contribution intérieure : M(<r) / r.
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
