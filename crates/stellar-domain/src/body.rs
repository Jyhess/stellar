use serde::{Deserialize, Serialize};

use crate::Vec3;

/// Stable identifier of a body. In a merger, the resulting body keeps the
/// identifier of the most massive member.
pub type BodyId = u64;

/// Set of bodies, stored as a "structure of arrays" (SoA): each attribute is
/// a contiguous vector, which helps vectorization and parallelism in the
/// engines.
///
/// Invariant: all vectors have the same length.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BodySet {
    pub ids: Vec<BodyId>,
    pub positions: Vec<Vec3>,
    pub velocities: Vec<Vec3>,
    pub masses: Vec<f64>,
}

impl BodySet {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            ids: Vec::with_capacity(n),
            positions: Vec::with_capacity(n),
            velocities: Vec::with_capacity(n),
            masses: Vec::with_capacity(n),
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn push(&mut self, id: BodyId, position: Vec3, velocity: Vec3, mass: f64) {
        self.ids.push(id);
        self.positions.push(position);
        self.velocities.push(velocity);
        self.masses.push(mass);
    }

    /// Checks the common-length invariant.
    pub fn is_consistent(&self) -> bool {
        let n = self.ids.len();
        self.positions.len() == n && self.velocities.len() == n && self.masses.len() == n
    }

    /// Index of the body with the given identifier (linear search).
    pub fn index_of(&self, id: BodyId) -> Option<usize> {
        self.ids.iter().position(|&x| x == id)
    }

    pub fn total_mass(&self) -> f64 {
        self.masses.iter().sum()
    }

    /// Total momentum Σ m·v.
    pub fn momentum(&self) -> Vec3 {
        self.masses
            .iter()
            .zip(&self.velocities)
            .map(|(&m, &v)| v * m)
            .sum()
    }

    /// Barycenter (position of the center of mass). `None` if the set is empty.
    pub fn center_of_mass(&self) -> Option<Vec3> {
        let m = self.total_mass();
        if m <= 0.0 {
            return None;
        }
        let s: Vec3 = self
            .masses
            .iter()
            .zip(&self.positions)
            .map(|(&mi, &p)| p * mi)
            .sum();
        Some(s / m)
    }

    /// Radius of the smallest sphere centred on `center` that contains at least
    /// the fraction `fraction` (between 0 and 1) of the total mass.
    pub fn mass_radius(&self, center: Vec3, fraction: f64) -> f64 {
        let total = self.total_mass();
        if total <= 0.0 {
            return 0.0;
        }
        let mut d: Vec<(f64, f64)> = self
            .positions
            .iter()
            .zip(&self.masses)
            .map(|(p, &m)| ((*p - center).length_squared(), m))
            .collect();
        d.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let target = fraction.clamp(0.0, 1.0) * total;
        let mut acc = 0.0;
        for (r2, m) in &d {
            acc += m;
            if acc >= target {
                return r2.sqrt();
            }
        }
        d.last().map_or(0.0, |x| x.0.sqrt())
    }

    /// Total kinetic energy Σ ½ m v².
    pub fn kinetic_energy(&self) -> f64 {
        self.masses
            .iter()
            .zip(&self.velocities)
            .map(|(&m, v)| 0.5 * m * v.length_squared())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mass_radius_weights_by_mass() {
        let mut b = BodySet::default();
        b.push(0, Vec3::new(1.0, 0.0, 0.0), Vec3::ZERO, 8.0);
        b.push(1, Vec3::new(0.0, 2.0, 0.0), Vec3::ZERO, 1.0);
        b.push(2, Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 1.0);
        assert_eq!(b.mass_radius(Vec3::ZERO, 0.8), 1.0);
        assert_eq!(b.mass_radius(Vec3::ZERO, 0.9), 2.0);
        assert_eq!(b.mass_radius(Vec3::ZERO, 1.0), 10.0);
    }
}
