//! Subsystems: a simulation split into clusters computed separately.
//!
//! An ejected cluster that has moved away becomes a separate subsystem,
//! computed by its own engine in its own frame (origin near its barycenter).
//! Subsystems advance with the same step; each one feels the others through
//! a **linear external field** ([`ExternalField`]): acceleration at the
//! barycenter and tidal tensor, derived from the masses and barycenters of
//! the others (monopole approximation of the sources, first-order expansion
//! around the target).
//!
//! Criteria (size / distance ratio, with hysteresis):
//! - a cluster found by the cleanup is split off when
//!   (cluster size + system size) < [`SPLIT_RATIO`] × distance;
//! - two subsystems are merged when
//!   (sum of sizes) > [`MERGE_RATIO`] × distance.
//!
//! A subsystem reduced to a single body that escapes the rest, and whose
//! influence on it is negligible ([`negligible_escapee`]), may be removed.

use std::any::Any;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{BodyId, Vec3};

/// Split threshold (size / distance).
pub const SPLIT_RATIO: f64 = 0.3;
/// Merge threshold (size / distance); well above the split threshold to
/// avoid going back and forth.
pub const MERGE_RATIO: f64 = 0.6;

/// Influence threshold of a negligible body: acceleration it gives the rest
/// of the system, relative to the rest's own acceleration scale.
pub const NEGLIGIBLE_INFLUENCE: f64 = 1e-6;

/// Is a lone body (mass `mass`, global position and velocity) negligible
/// with respect to the rest of the simulation (moments `rest`, size
/// `rest_size`)? It must be on an escape trajectory (positive energy
/// relative to the rest, seen as a point mass), moving away from it, and its
/// influence (m / M) · (R / d)² — acceleration G·m/d² it gives the rest,
/// relative to the rest's own scale G·M/R² — below [`NEGLIGIBLE_INFLUENCE`].
/// The criterion depends on distance: a heavy body far enough away
/// qualifies.
pub fn negligible_escapee(
    g: f64,
    mass: f64,
    position: Vec3,
    velocity: Vec3,
    rest: &Moments,
    rest_size: f64,
) -> bool {
    if rest.mass <= 0.0 {
        return false;
    }
    let r = position - rest.center;
    let v = velocity - rest.velocity;
    let d = r.length();
    if d <= 0.0 || r.dot(v) <= 0.0 {
        return false;
    }
    let escaping = 0.5 * v.length_squared() > g * (rest.mass + mass) / d;
    let influence = (mass / rest.mass) * (rest_size / d).powi(2);
    escaping && influence < NEGLIGIBLE_INFLUENCE
}

/// External gravitational field, linearized around `center` (engine
/// coordinates): a(x) = `acceleration` + `tidal` · (x − `center`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ExternalField {
    pub center: Vec3,
    pub acceleration: Vec3,
    /// Tidal tensor (rows), symmetric and traceless.
    pub tidal: [[f64; 3]; 3],
}

impl ExternalField {
    /// Acceleration at point `x`.
    pub fn at(&self, x: Vec3) -> Vec3 {
        let d = x - self.center;
        let t = &self.tidal;
        self.acceleration
            + Vec3::new(
                t[0][0] * d.x + t[0][1] * d.y + t[0][2] * d.z,
                t[1][0] * d.x + t[1][1] * d.y + t[1][2] * d.z,
                t[2][0] * d.x + t[2][1] * d.y + t[2][2] * d.z,
            )
    }

    /// Field created at `target` by a point mass `mass` located at `source`
    /// (Plummer softening `eps2`), linearized around `target`. Any coordinates,
    /// as long as both points share them.
    pub fn of_point_mass(g: f64, mass: f64, source: Vec3, target: Vec3, eps2: f64) -> Self {
        let r = target - source;
        let d2 = r.length_squared() + eps2;
        let inv = 1.0 / d2.sqrt();
        let inv3 = inv * inv * inv;
        let inv5 = inv3 * inv * inv;
        let gm = g * mass;
        let ra = [r.x, r.y, r.z];
        let mut tidal = [[0.0; 3]; 3];
        for (i, row) in tidal.iter_mut().enumerate() {
            for (j, t) in row.iter_mut().enumerate() {
                let delta = if i == j { 1.0 } else { 0.0 };
                *t = -gm * (delta * inv3 - 3.0 * ra[i] * ra[j] * inv5);
            }
        }
        Self {
            center: target,
            acceleration: -r * (gm * inv3),
            tidal,
        }
    }

    /// Sum of two fields expanded around the same center.
    pub fn add(&mut self, other: &Self) {
        self.acceleration += other.acceleration;
        for i in 0..3 {
            for j in 0..3 {
                self.tidal[i][j] += other.tidal[i][j];
            }
        }
    }

    /// The same field, expressed in a frame shifted by `-shift`.
    pub fn shifted(mut self, shift: Vec3) -> Self {
        self.center -= shift;
        self
    }
}

/// Mass, barycenter and barycenter velocity of a system (engine
/// coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Moments {
    pub mass: f64,
    pub center: Vec3,
    pub velocity: Vec3,
}

/// Range of a subsystem's bodies in a composite state (bodies grouped by
/// subsystem), and its characteristic size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SystemSpan {
    pub start: usize,
    pub len: usize,
    /// Characteristic size (reference radius of the cleanup and of the split
    /// and merge criteria).
    pub size: f64,
}

/// Summary of a subsystem, for display (global coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemSummary {
    /// Marker body (the most massive one at the time of the split).
    pub anchor: BodyId,
    pub bodies: usize,
    pub mass: f64,
    pub center: Vec3,
    pub velocity: Vec3,
    pub size: f64,
}

/// Element of a composite live view: direct access to an engine (see
/// [`crate::PhysicsEngine::live_view`]) and origin of its frame.
#[derive(Clone)]
pub struct ViewPart {
    pub view: Arc<dyn Any + Send + Sync>,
    pub origin: Vec3,
}

/// Live view of several subsystems: the list changes when subsystems are
/// born, merge or are recentred.
#[derive(Default)]
pub struct SystemsView {
    parts: Mutex<Arc<Vec<ViewPart>>>,
}

impl SystemsView {
    pub fn parts(&self) -> Arc<Vec<ViewPart>> {
        Arc::clone(&self.parts.lock().expect("poisoned lock"))
    }

    pub fn set(&self, parts: Vec<ViewPart>) {
        *self.parts.lock().expect("poisoned lock") = Arc::new(parts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rest() -> Moments {
        Moments {
            mass: 1e6,
            center: Vec3::ZERO,
            velocity: Vec3::ZERO,
        }
    }

    #[test]
    fn negligible_escapee_needs_escape_recession_and_low_influence() {
        let (g, size) = (1.0, 10.0);
        // Fast, receding, far: (1 / 1e6) · (10 / 1e4)² = 1e-12.
        let far = Vec3::new(1e4, 0.0, 0.0);
        let out = Vec3::new(100.0, 0.0, 0.0);
        assert!(negligible_escapee(g, 1.0, far, out, &rest(), size));
        // Coming back: kept.
        assert!(!negligible_escapee(g, 1.0, far, -out, &rest(), size));
        // Bound (too slow to escape): kept.
        assert!(!negligible_escapee(g, 1.0, far, Vec3::new(1.0, 0.0, 0.0), &rest(), size));
        // Heavy and close: (1e4 / 1e6) · (10 / 1e3)² = 1e-6, not below the threshold.
        let near = Vec3::new(1e3, 0.0, 0.0);
        assert!(!negligible_escapee(g, 1e4, near, out, &rest(), size));
        // The same heavy body, ten times farther: 1e-8, removed.
        assert!(negligible_escapee(g, 1e4, near * 10.0, out, &rest(), size));
        // Nothing left to compare with.
        assert!(!negligible_escapee(g, 1.0, far, out, &Moments::default(), size));
    }
}
