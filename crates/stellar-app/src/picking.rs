//! Selection of a body under an aiming ray.

use glam::DVec3;
use stellar_domain::physics::body_radius;
use stellar_domain::{ranking, BodyId, BodySet};

use crate::camera::Camera;

/// Body aimed at by the ray (`origin`, unit `dir`): among the bodies whose
/// angular distance to the ray is below the tolerance (increased by their
/// apparent radius), the most massive one is kept, then the best centred.
pub fn pick(
    bodies: &BodySet,
    origin: DVec3,
    dir: DVec3,
    tolerance_rad: f64,
    base_radius: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64, f64)> = None; // (index, mass, angle)
    for i in 0..bodies.len() {
        let v = bodies.positions[i] - origin;
        let t = v.dot(dir);
        if t <= 0.0 {
            continue;
        }
        let perp = (v.length_squared() - t * t).max(0.0).sqrt();
        let angle = perp / t;
        let m = bodies.masses[i];
        let apparent = body_radius(m, base_radius) / t;
        if angle > tolerance_rad + apparent {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, bm, ba)) => m > bm || (m == bm && angle < ba),
        };
        if better {
            best = Some((i, m, angle));
        }
    }
    best.map(|b| b.0)
}

/// The `k` most massive bodies among those visible on screen (in the
/// camera's field of view, in front of it), from heaviest to lightest.
pub fn visible_top_k(bodies: &BodySet, camera: &Camera, aspect: f64, k: usize) -> Vec<BodyId> {
    let mut ids = Vec::new();
    let mut masses = Vec::new();
    for i in 0..bodies.len() {
        if let Some((x, y)) = camera.project(bodies.positions[i], aspect) {
            if x.abs() <= 1.0 && y.abs() <= 1.0 {
                ids.push(bodies.ids[i]);
                masses.push(bodies.masses[i]);
            }
        }
    }
    ranking::top_k(&ids, &masses, k)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_top_k_ignores_bodies_out_of_view() {
        let mut b = BodySet::default();
        b.push(1, DVec3::new(10.0, 0.0, 0.0), DVec3::ZERO, 5.0); // in front
        b.push(2, DVec3::new(-10.0, 0.0, 0.0), DVec3::ZERO, 100.0); // behind
        b.push(3, DVec3::new(10.0, 50.0, 0.0), DVec3::ZERO, 90.0); // out of view
        b.push(4, DVec3::new(10.0, 1.0, 0.5), DVec3::ZERO, 7.0); // in front
        let cam = Camera::looking_at(DVec3::ZERO, DVec3::X, 1.0);
        assert_eq!(visible_top_k(&b, &cam, 1.0, 10), vec![4, 1]);
        assert_eq!(visible_top_k(&b, &cam, 1.0, 1), vec![4]);
    }

    #[test]
    fn prefers_massive_body_within_tolerance() {
        let mut b = BodySet::default();
        b.push(1, DVec3::new(10.0, 0.0, 0.0), DVec3::ZERO, 1.0);
        b.push(2, DVec3::new(10.0, 0.2, 0.0), DVec3::ZERO, 50.0);
        b.push(3, DVec3::new(10.0, 5.0, 0.0), DVec3::ZERO, 1000.0);
        let i = pick(&b, DVec3::ZERO, DVec3::X, 0.05, 0.0).unwrap();
        assert_eq!(b.ids[i], 2);
        assert!(pick(&b, DVec3::ZERO, -DVec3::X, 0.05, 0.0).is_none());
    }
}
