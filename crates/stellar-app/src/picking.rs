//! Sélection d'un corps sous un rayon de visée.

use glam::DVec3;
use stellar_domain::physics::body_radius;
use stellar_domain::BodySet;

/// Corps visé par le rayon (`origin`, `dir` unitaire) : parmi les corps dont
/// l'écart angulaire au rayon est inférieur à la tolérance (augmentée de leur
/// rayon apparent), on retient le plus massif, puis le mieux centré.
pub fn pick(
    bodies: &BodySet,
    origin: DVec3,
    dir: DVec3,
    tolerance_rad: f64,
    base_radius: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64, f64)> = None; // (indice, masse, angle)
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

#[cfg(test)]
mod tests {
    use super::*;

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
