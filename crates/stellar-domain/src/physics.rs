//! Règles physiques élémentaires, indépendantes de tout moteur.

use crate::{BodyId, Vec3};

/// Rayon d'un corps de masse `mass` : densité constante, r = r₀·m^(1/3).
#[inline]
pub fn body_radius(mass: f64, base_radius: f64) -> f64 {
    base_radius * mass.cbrt()
}

/// Distance minimale entre deux corps en mouvement rectiligne uniforme sur un
/// pas, connaissant leurs positions de début (`a0`, `b0`) et de fin (`a1`, `b1`).
///
/// C'est le cœur de la détection de contact « balayée » : deux corps rapides
/// qui se croisent entre deux pas sont bien détectés.
#[inline]
pub fn swept_min_distance(a0: Vec3, a1: Vec3, b0: Vec3, b1: Vec3) -> f64 {
    let d0 = b0 - a0; // séparation initiale
    let dv = (b1 - b0) - (a1 - a0); // variation de la séparation sur le pas
    let dv2 = dv.length_squared();
    let t = if dv2 > 0.0 {
        (-d0.dot(dv) / dv2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (d0 + dv * t).length()
}

/// Corps élémentaire manipulé lors d'une fusion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MergeInput {
    pub id: BodyId,
    pub mass: f64,
    pub position: Vec3,
    pub velocity: Vec3,
}

/// Fusion d'un groupe de corps (au moins un) :
/// - la masse est la somme des masses ;
/// - la position est le barycentre ;
/// - la quantité de mouvement est conservée ;
/// - l'identifiant est celui du membre le plus massif (à masse égale, le plus
///   petit identifiant).
pub fn merge(group: &[MergeInput]) -> MergeInput {
    assert!(!group.is_empty(), "fusion d'un groupe vide");
    let mut mass = 0.0;
    let mut mx = Vec3::ZERO;
    let mut mv = Vec3::ZERO;
    let mut survivor = group[0];
    for b in group {
        mass += b.mass;
        mx += b.position * b.mass;
        mv += b.velocity * b.mass;
        if b.mass > survivor.mass || (b.mass == survivor.mass && b.id < survivor.id) {
            survivor = *b;
        }
    }
    MergeInput {
        id: survivor.id,
        mass,
        position: mx / mass,
        velocity: mv / mass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_scales_with_cube_root_of_mass() {
        assert!((body_radius(8.0, 0.5) - 1.0).abs() < 1e-12);
        assert!((body_radius(1.0, 0.5) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn swept_distance_detects_crossing() {
        // Deux corps qui se croisent au milieu du pas.
        let d = swept_min_distance(
            Vec3::new(-10.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(10.0, 0.1, 0.0),
            Vec3::new(-10.0, 0.1, 0.0),
        );
        assert!((d - 0.1).abs() < 1e-12);
    }

    #[test]
    fn swept_distance_clamps_to_segment() {
        // S'éloignent : la distance minimale est la distance initiale.
        let d = swept_min_distance(
            Vec3::ZERO,
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
        );
        assert!((d - 2.0).abs() < 1e-12);
    }

    #[test]
    fn merge_conserves_mass_and_momentum() {
        let g = [
            MergeInput { id: 7, mass: 1.0, position: Vec3::X, velocity: Vec3::Y },
            MergeInput { id: 3, mass: 3.0, position: -Vec3::X, velocity: -Vec3::Y },
        ];
        let m = merge(&g);
        assert_eq!(m.id, 3);
        assert_eq!(m.mass, 4.0);
        assert!((m.position - Vec3::new(-0.5, 0.0, 0.0)).length() < 1e-12);
        let p_before: Vec3 = g.iter().map(|b| b.velocity * b.mass).sum();
        assert!((m.velocity * m.mass - p_before).length() < 1e-12);
    }

    #[test]
    fn merge_tie_keeps_smallest_id() {
        let g = [
            MergeInput { id: 9, mass: 1.0, position: Vec3::ZERO, velocity: Vec3::ZERO },
            MergeInput { id: 2, mass: 1.0, position: Vec3::ZERO, velocity: Vec3::ZERO },
        ];
        assert_eq!(merge(&g).id, 2);
    }
}
