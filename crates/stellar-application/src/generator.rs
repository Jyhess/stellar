//! Deterministic generation of the initial state from the
//! [`InitialConditions`] (same seed ⇒ same cloud).

use rand::{Rng, SeedableRng};
use rand_pcg::Pcg64Mcg;
use stellar_domain::{
    BodySet, DensityProfile, Distribution, InitialConditions, SimulationState, Vec3, VelocityMode,
};

/// Offset of the seed of the dust masses: a stream of its own, so that the
/// positions do not depend on the mass spectrum.
const MASS_STREAM: u64 = 0x9E37_79B9_7F4A_7C15;

/// Builds the initial state: dust (masses from the spectrum), optional sun
/// and planets, barycenter at the origin, zero total momentum.
pub fn generate_state(ic: &InitialConditions, g: f64) -> SimulationState {
    let bodies = generate_bodies(ic, g);
    let initial_mass = Some(bodies.total_mass());
    SimulationState {
        bodies,
        step: 0,
        time: 0.0,
        initial_energy: None,
        initial_mass,
        systems: Vec::new(),
    }
}

pub fn generate_bodies(ic: &InitialConditions, g: f64) -> BodySet {
    let n = ic.count;
    let mut rng = Pcg64Mcg::seed_from_u64(ic.seed);
    let mut masses = DustMassDraw::new(ic);
    let mut bodies = BodySet::with_capacity(ic.body_count());

    let mut dust_mass = 0.0;
    for i in 0..n {
        let p = sample_position(&mut rng, ic);
        let m = masses.next();
        dust_mass += m;
        bodies.push(i as u64, p, Vec3::ZERO, m);
    }
    let (sun, planet) = massive_masses(ic, dust_mass);
    let mut id = n as u64;
    if let Some(m) = sun {
        bodies.push(id, Vec3::ZERO, Vec3::ZERO, m);
        id += 1;
    }
    for k in 0..ic.planets.count {
        let r = planet_radius(ic, k);
        let phi: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        bodies.push(id, Vec3::new(r * phi.cos(), r * phi.sin(), 0.0), Vec3::ZERO, planet);
        id += 1;
    }
    recenter_positions(&mut bodies);
    assign_velocities(&mut rng, ic, g, &mut bodies);
    remove_net_momentum(&mut bodies);
    bodies
}

/// Exact total mass of the bodies generated from `ic`, without generating
/// them (draws only the dust masses).
pub fn initial_total_mass(ic: &InitialConditions) -> f64 {
    let mut masses = DustMassDraw::new(ic);
    let dust_mass: f64 = (0..ic.count).map(|_| masses.next()).sum();
    let (sun, planet) = massive_masses(ic, dust_mass);
    dust_mass + sun.unwrap_or(0.0) + ic.planets.count as f64 * planet
}

/// Draw of the dust masses, on a stream of its own.
struct DustMassDraw {
    rng: Option<Pcg64Mcg>,
    spectrum: stellar_domain::DustMasses,
}

impl DustMassDraw {
    fn new(ic: &InitialConditions) -> Self {
        Self {
            // Mass 1 for all: no draw.
            rng: (ic.dust.max > 1).then(|| Pcg64Mcg::seed_from_u64(ic.seed ^ MASS_STREAM)),
            spectrum: ic.dust,
        }
    }

    fn next(&mut self) -> f64 {
        match &mut self.rng {
            Some(rng) => self.spectrum.sample(rng.gen()),
            None => 1.0,
        }
    }
}

/// Masses of the sun and of each planet (integers, at least 1), from their
/// share of the total mass.
fn massive_masses(ic: &InitialConditions, dust_mass: f64) -> (Option<f64>, f64) {
    let total = ic.total_mass_for_dust(dust_mass);
    let mass = |share: f64| (share.max(0.0) * total).round().max(1.0);
    (ic.sun.map(mass), mass(ic.planets.mass_fraction))
}

/// Orbital radius of the planet `k`: evenly spaced between the inner and
/// outer radii (in the middle for a single planet).
fn planet_radius(ic: &InitialConditions, k: u32) -> f64 {
    let p = ic.planets;
    let t = if p.count > 1 {
        k as f64 / (p.count - 1) as f64
    } else {
        0.5
    };
    (p.inner + (p.outer - p.inner) * t) * ic.radius
}

fn unit_vector(rng: &mut impl Rng) -> Vec3 {
    let z: f64 = rng.gen_range(-1.0..=1.0);
    let phi: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
    let s = (1.0 - z * z).max(0.0).sqrt();
    Vec3::new(s * phi.cos(), s * phi.sin(), z)
}

fn uniform(rng: &mut impl Rng, min: f64, max: f64) -> f64 {
    let (a, b) = if min <= max { (min, max) } else { (max, min) };
    if a == b {
        a
    } else {
        rng.gen_range(a..b)
    }
}

fn sample_position(rng: &mut impl Rng, ic: &InitialConditions) -> Vec3 {
    let r_max = ic.radius;
    match ic.distribution {
        Distribution::Sphere => {
            let r = r_max * rng.gen::<f64>().cbrt();
            unit_vector(rng) * r
        }
        Distribution::Disk { thickness, profile } => {
            let rho = match profile {
                DensityProfile::Uniform => r_max * rng.gen::<f64>().sqrt(),
                DensityProfile::Exponential { scale } => {
                    // Surface density ∝ exp(−r/h) ⇒ r ~ Gamma(2, h), truncated at R.
                    let h = (scale.max(1e-6)) * r_max;
                    loop {
                        let u1: f64 = rng.gen::<f64>().max(f64::MIN_POSITIVE);
                        let u2: f64 = rng.gen::<f64>().max(f64::MIN_POSITIVE);
                        let r = -h * (u1 * u2).ln();
                        if r <= r_max {
                            break r;
                        }
                    }
                }
            };
            let phi: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
            let z = (rng.gen::<f64>() - 0.5) * thickness.max(0.0) * r_max;
            Vec3::new(rho * phi.cos(), rho * phi.sin(), z)
        }
    }
}

fn recenter_positions(b: &mut BodySet) {
    if let Some(c) = b.center_of_mass() {
        for p in &mut b.positions {
            *p -= c;
        }
    }
}

fn remove_net_momentum(b: &mut BodySet) {
    let m = b.total_mass();
    if m > 0.0 {
        let v_cm = b.momentum() / m;
        for v in &mut b.velocities {
            *v -= v_cm;
        }
    }
}

/// Unit tangent of a rotation around Oz (counter-clockwise).
fn azimuthal(p: Vec3) -> Option<Vec3> {
    let rho = (p.x * p.x + p.y * p.y).sqrt();
    (rho > 0.0).then(|| Vec3::new(-p.y / rho, p.x / rho, 0.0))
}

/// Mass enclosed by the sphere through each body (the body excluded): rank
/// in the sort by spherical radius.
fn enclosed_masses(b: &BodySet) -> Vec<f64> {
    let n = b.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by(|&i, &j| {
        b.positions[i]
            .length_squared()
            .total_cmp(&b.positions[j].length_squared())
    });
    let mut enclosed = vec![0.0f64; n];
    let mut acc = 0.0;
    for &i in &order {
        enclosed[i] = acc;
        acc += b.masses[i];
    }
    enclosed
}

/// Equilibrium speed of a rotation around Oz under the enclosed mass, with
/// its direction (`None` on the axis).
fn circular_velocity(p: Vec3, inside: f64, g: f64) -> Option<Vec3> {
    let r2 = p.length_squared();
    let rho2 = p.x * p.x + p.y * p.y;
    if r2 <= 0.0 {
        return None;
    }
    let t = azimuthal(p)?;
    // Cylindrical component of the attraction: g_ρ = G·M(<r)·ρ / r³
    // ⇒ v = √(ρ·g_ρ) = √(G·M(<r)·ρ² / r³).
    Some(t * (g * inside * rho2 / (r2 * r2.sqrt())).sqrt())
}

/// Velocities of the dust (the first `ic.count` bodies) according to the
/// chosen mode; the sun stays at rest and the planets are on circular orbits.
fn assign_velocities(rng: &mut impl Rng, ic: &InitialConditions, g: f64, b: &mut BodySet) {
    let n = ic.count;
    let orbital = matches!(ic.velocity, VelocityMode::CircularOrbital { .. });
    let enclosed = (orbital || ic.planets.count > 0).then(|| enclosed_masses(b));
    match ic.velocity {
        VelocityMode::Rest => {}
        VelocityMode::Random { min, max } => {
            for v in &mut b.velocities[..n] {
                *v = unit_vector(rng) * uniform(rng, min, max);
            }
        }
        VelocityMode::CircularFixed { min, max } => {
            for (p, v) in b.positions[..n].iter().zip(&mut b.velocities[..n]) {
                if let Some(t) = azimuthal(*p) {
                    *v = t * uniform(rng, min, max);
                }
            }
        }
        VelocityMode::Radial { min, max } => {
            for (p, v) in b.positions[..n].iter().zip(&mut b.velocities[..n]) {
                let r = p.length();
                if r > 0.0 {
                    *v = -*p / r * uniform(rng, min, max);
                }
            }
        }
        VelocityMode::CircularOrbital { spread, factor: scale } => {
            let enclosed = enclosed.as_deref().unwrap_or_default();
            let spread = spread.abs();
            for (i, &inside) in enclosed[..n].iter().enumerate() {
                let Some(v_circ) = circular_velocity(b.positions[i], inside, g) else {
                    continue;
                };
                let factor = if spread > 0.0 {
                    1.0 + rng.gen_range(-spread..spread)
                } else {
                    1.0
                };
                b.velocities[i] = v_circ * scale * factor;
            }
        }
    }
    if let Some(enclosed) = &enclosed {
        let first = n + usize::from(ic.sun.is_some());
        for (i, &inside) in enclosed.iter().enumerate().skip(first) {
            if let Some(v) = circular_velocity(b.positions[i], inside, g) {
                b.velocities[i] = v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ic(distribution: Distribution, velocity: VelocityMode) -> InitialConditions {
        InitialConditions {
            count: 5_000,
            radius: 50.0,
            seed: 7,
            distribution,
            velocity,
            ..InitialConditions::default()
        }
    }

    #[test]
    fn dust_masses_follow_the_spectrum() {
        let mut c = ic(Distribution::Sphere, VelocityMode::Rest);
        c.count = 50_000;
        c.dust = stellar_domain::DustMasses {
            max: 20,
            exponent: -2.0,
        };
        let b = generate_bodies(&c, 1.0);
        assert!(b
            .masses
            .iter()
            .all(|&m| (1.0..=20.0).contains(&m) && m.fract() == 0.0));
        // Many small grains, few big ones.
        let ones = b.masses.iter().filter(|&&m| m == 1.0).count();
        let twenties = b.masses.iter().filter(|&&m| m == 20.0).count();
        assert!(ones > 20 * twenties.max(1), "{ones} vs {twenties}");
        let mean = b.total_mass() / c.count as f64;
        let expected = c.dust.mean();
        assert!((mean - expected).abs() / expected < 0.02, "{mean} vs {expected}");
        assert_eq!(b.total_mass(), initial_total_mass(&c));

        // The positions do not depend on the spectrum (up to the recentring).
        let mut plain = c;
        plain.dust.max = 1;
        let p = generate_bodies(&plain, 1.0).positions;
        let shift = p[0] - b.positions[0];
        assert!(p
            .iter()
            .zip(&b.positions)
            .all(|(a, b)| (*a - *b - shift).length() < 1e-9));
    }

    #[test]
    fn spectrum_mean_and_sampling_agree() {
        for exponent in [-2.0, -1.0, 0.0, 1.5] {
            let d = stellar_domain::DustMasses { max: 10, exponent };
            let n = 200_000;
            let sampled: f64 =
                (0..n).map(|k| d.sample((k as f64 + 0.5) / n as f64)).sum::<f64>() / n as f64;
            assert!((sampled - d.mean()).abs() < 1e-3 * d.mean(), "{exponent}: {sampled} vs {}", d.mean());
        }
        assert_eq!(stellar_domain::DustMasses::default().mean(), 1.0);
    }

    #[test]
    fn sun_and_planets() {
        let mut c = ic(
            Distribution::Disk {
                thickness: 0.02,
                profile: DensityProfile::Uniform,
            },
            VelocityMode::CircularOrbital { spread: 0.0, factor: 1.0 },
        );
        c.sun = Some(0.5);
        c.planets = stellar_domain::Planets {
            count: 4,
            mass_fraction: 0.01,
            inner: 0.2,
            outer: 0.8,
        };
        let g = 2.0;
        let b = generate_bodies(&c, g);
        assert_eq!(b.len(), c.body_count());
        assert!(b.is_consistent());
        let total = b.total_mass();
        assert_eq!(total, initial_total_mass(&c));

        let sun = c.count;
        assert_eq!(b.ids[sun], c.count as u64);
        assert!((b.masses[sun] / total - 0.5).abs() < 1e-3);
        let center = b.positions[sun];
        assert!(center.length() < 0.05 * c.radius);

        for k in 0..4 {
            let i = sun + 1 + k;
            assert!((b.masses[i] / total - 0.01).abs() < 1e-3);
            let rel = b.positions[i] - center;
            let expected_r = (0.2 + 0.2 * k as f64) * c.radius;
            assert!((rel.length() - expected_r).abs() < 0.05 * c.radius, "{k}");
            assert!(rel.z.abs() < 0.05 * c.radius);
            // Circular orbit: dominated by the sun and the inner dust.
            let v = (b.velocities[i] - b.velocities[sun]).length();
            let inside: f64 = (0..b.len())
                .filter(|&j| j != i && b.positions[j].length() < b.positions[i].length())
                .map(|j| b.masses[j])
                .sum();
            let expected = (g * inside / b.positions[i].length()).sqrt();
            assert!((v - expected).abs() / expected < 0.05, "{k}: {v} vs {expected}");
        }
        assert!(b.momentum().length() < 1e-6 * total);
    }

    #[test]
    fn planets_orbit_whatever_the_dust_mode() {
        let mut c = ic(Distribution::Sphere, VelocityMode::Rest);
        c.sun = Some(0.9);
        c.planets.count = 1;
        let b = generate_bodies(&c, 1.0);
        let planet = c.count + 1;
        let sun = c.count;
        let v = (b.velocities[planet] - b.velocities[sun]).length();
        assert!(v > 0.0);
        // Dust at rest (up to the momentum correction).
        assert!(b.velocities[0].length() < 0.1 * v);
    }

    #[test]
    fn deterministic_with_seed() {
        let c = ic(
            Distribution::Sphere,
            VelocityMode::Random { min: 0.0, max: 2.0 },
        );
        assert_eq!(generate_bodies(&c, 1.0), generate_bodies(&c, 1.0));
        let mut c2 = c;
        c2.seed = 8;
        assert_ne!(generate_bodies(&c, 1.0), generate_bodies(&c2, 1.0));
    }

    #[test]
    fn sphere_within_radius_and_centered() {
        let c = ic(Distribution::Sphere, VelocityMode::Rest);
        let b = generate_bodies(&c, 1.0);
        assert_eq!(b.len(), 5_000);
        assert!(b.is_consistent());
        // Recentring: a slight, bounded overflow is possible.
        assert!(b.positions.iter().all(|p| p.length() <= 50.0 * 1.05));
        assert!(b.center_of_mass().unwrap().length() < 1e-9);
        assert!(b.masses.iter().all(|&m| m == 1.0));
    }

    #[test]
    fn disk_is_flat() {
        let c = ic(
            Distribution::Disk {
                thickness: 0.02,
                profile: DensityProfile::Exponential { scale: 0.25 },
            },
            VelocityMode::Rest,
        );
        let b = generate_bodies(&c, 1.0);
        assert!(b
            .positions
            .iter()
            .all(|p| p.z.abs() <= 0.5 * 0.02 * 50.0 * 1.1));
    }

    #[test]
    fn net_momentum_is_zero() {
        for v in [
            VelocityMode::Random { min: 1.0, max: 3.0 },
            VelocityMode::CircularFixed { min: 1.0, max: 2.0 },
            VelocityMode::CircularOrbital { spread: 0.1, factor: 0.8 },
            VelocityMode::Radial { min: 0.5, max: 1.0 },
        ] {
            let b = generate_bodies(&ic(Distribution::Sphere, v), 1.0);
            assert!(b.momentum().length() < 1e-6, "{v:?}");
        }
    }

    #[test]
    fn orbital_speed_matches_kepler_in_disk() {
        let c = ic(
            Distribution::Disk {
                thickness: 0.0,
                profile: DensityProfile::Uniform,
            },
            VelocityMode::CircularOrbital { spread: 0.0, factor: 1.0 },
        );
        let b = generate_bodies(&c, 2.0);
        // For the outermost body, M(<r) ≈ N − 1.
        let (i, r) = b
            .positions
            .iter()
            .map(|p| p.length())
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let expected = (2.0 * (c.count as f64 - 1.0) / r).sqrt();
        let got = b.velocities[i].length();
        assert!(
            (got - expected).abs() / expected < 0.05,
            "{got} vs {expected}"
        );

        // The factor multiplies the equilibrium speed.
        let slow = InitialConditions {
            velocity: VelocityMode::CircularOrbital { spread: 0.0, factor: 0.7 },
            ..c
        };
        let s = generate_bodies(&slow, 2.0);
        let ratio = s.velocities[i].length() / got;
        assert!((ratio - 0.7).abs() < 1e-9, "{ratio}");
    }
}
