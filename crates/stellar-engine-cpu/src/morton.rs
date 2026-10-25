//! Codes de Morton 3D sur 63 bits (21 bits par axe).

use glam::DVec3;

pub const BITS_PER_AXIS: u32 = 21;
const AXIS_CELLS: f64 = (1u64 << BITS_PER_AXIS) as f64;
const AXIS_MAX: u64 = (1u64 << BITS_PER_AXIS) - 1;

/// Étale les 21 bits de poids faible de `v` un bit sur trois.
#[inline]
pub fn spread_bits(v: u64) -> u64 {
    let mut x = v & 0x1f_ffff;
    x = (x | x << 32) & 0x001f_0000_0000_ffff;
    x = (x | x << 16) & 0x001f_0000_ff00_00ff;
    x = (x | x << 8) & 0x100f_00f0_0f00_f00f;
    x = (x | x << 4) & 0x10c3_0c30_c30c_30c3;
    x = (x | x << 2) & 0x1249_2492_4924_9249;
    x
}

/// Code de Morton de coordonnées entières (x sur le bit 0 de chaque triplet,
/// y sur le bit 1, z sur le bit 2).
#[inline]
pub fn encode(ix: u64, iy: u64, iz: u64) -> u64 {
    spread_bits(ix) | (spread_bits(iy) << 1) | (spread_bits(iz) << 2)
}

/// Cube englobant : coin minimal et côté.
#[derive(Debug, Clone, Copy)]
pub struct Cube {
    pub min: DVec3,
    pub side: f64,
}

impl Cube {
    /// Plus petit cube (légèrement dilaté) contenant tous les points.
    pub fn bounding(points: &[DVec3]) -> Self {
        use rayon::prelude::*;
        let (lo, hi) = points
            .par_iter()
            .fold(
                || (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
                |(lo, hi), &p| (lo.min(p), hi.max(p)),
            )
            .reduce(
                || (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
                |a, b| (a.0.min(b.0), a.1.max(b.1)),
            );
        if !lo.is_finite() || !hi.is_finite() {
            return Self { min: DVec3::ZERO, side: 1.0 };
        }
        let side = (hi - lo).max_element().max(1e-9) * (1.0 + 1e-9) + 1e-12;
        Self { min: lo, side }
    }

    /// Code de Morton d'un point du cube.
    #[inline]
    pub fn code(&self, p: DVec3) -> u64 {
        let t = (p - self.min) / self.side * AXIS_CELLS;
        let q = |v: f64| (v.max(0.0) as u64).min(AXIS_MAX);
        encode(q(t.x), q(t.y), q(t.z))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_interleaves() {
        assert_eq!(encode(1, 0, 0), 0b001);
        assert_eq!(encode(0, 1, 0), 0b010);
        assert_eq!(encode(0, 0, 1), 0b100);
        assert_eq!(encode(3, 0, 0), 0b001_001);
        assert_eq!(encode(AXIS_MAX, AXIS_MAX, AXIS_MAX), (1u64 << 63) - 1);
    }
}
