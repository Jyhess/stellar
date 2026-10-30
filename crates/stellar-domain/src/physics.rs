//! Elementary physical rules, independent of any engine.

use crate::{BodyId, Vec3};

/// Radius of a body of mass `mass`: constant density, r = r₀·m^(1/3).
#[inline]
pub fn body_radius(mass: f64, base_radius: f64) -> f64 {
    base_radius * mass.cbrt()
}

/// Minimum distance between two bodies in uniform straight-line motion over a
/// step, given their start (`a0`, `b0`) and end (`a1`, `b1`) positions.
///
/// This is the core of "swept" contact detection: two fast bodies that
/// cross between two steps are still detected.
#[inline]
pub fn swept_min_distance(a0: Vec3, a1: Vec3, b0: Vec3, b1: Vec3) -> f64 {
    let d0 = b0 - a0; // initial separation
    let dv = (b1 - b0) - (a1 - a0); // change of the separation over the step
    let dv2 = dv.length_squared();
    let t = if dv2 > 0.0 {
        (-d0.dot(dv) / dv2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (d0 + dv * t).length()
}

/// Elementary body handled during a merger.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MergeInput {
    pub id: BodyId,
    pub mass: f64,
    pub position: Vec3,
    pub velocity: Vec3,
}

/// Merger of a group of bodies (at least one):
/// - the mass is the sum of the masses;
/// - the position is the barycenter;
/// - momentum is conserved;
/// - the identifier is that of the most massive member (for equal masses,
///   the smallest identifier).
pub fn merge(group: &[MergeInput]) -> MergeInput {
    assert!(!group.is_empty(), "merge of an empty group");
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
        // Two bodies that cross in the middle of the step.
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
        // Moving apart: the minimum distance is the initial distance.
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
            MergeInput {
                id: 7,
                mass: 1.0,
                position: Vec3::X,
                velocity: Vec3::Y,
            },
            MergeInput {
                id: 3,
                mass: 3.0,
                position: -Vec3::X,
                velocity: -Vec3::Y,
            },
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
            MergeInput {
                id: 9,
                mass: 1.0,
                position: Vec3::ZERO,
                velocity: Vec3::ZERO,
            },
            MergeInput {
                id: 2,
                mass: 1.0,
                position: Vec3::ZERO,
                velocity: Vec3::ZERO,
            },
        ];
        assert_eq!(merge(&g).id, 2);
    }
}
