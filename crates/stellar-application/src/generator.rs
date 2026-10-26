//! Génération déterministe de l'état initial à partir des
//! [`InitialConditions`] (même graine ⇒ même nuage).

use rand::{Rng, SeedableRng};
use rand_pcg::Pcg64Mcg;
use stellar_domain::{
    BodySet, DensityProfile, Distribution, InitialConditions, SimulationState, Vec3, VelocityMode,
};

/// Construit l'état initial : particules de masse 1, barycentre à l'origine,
/// quantité de mouvement totale nulle.
pub fn generate_state(ic: &InitialConditions, g: f64) -> SimulationState {
    let bodies = generate_bodies(ic, g);
    let initial_mass = Some(bodies.total_mass());
    SimulationState {
        bodies,
        step: 0,
        time: 0.0,
        initial_energy: None,
        initial_mass,
    }
}

pub fn generate_bodies(ic: &InitialConditions, g: f64) -> BodySet {
    let n = ic.count;
    let mut rng = Pcg64Mcg::seed_from_u64(ic.seed);
    let mut bodies = BodySet::with_capacity(n);

    for i in 0..n {
        let p = sample_position(&mut rng, ic);
        bodies.push(i as u64, p, Vec3::ZERO, 1.0);
    }
    recenter_positions(&mut bodies);
    assign_velocities(&mut rng, ic, g, &mut bodies);
    remove_net_momentum(&mut bodies);
    bodies
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
                    // Densité surfacique ∝ exp(−r/h) ⇒ r ~ Gamma(2, h), tronquée à R.
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

/// Tangente unitaire à une rotation autour de Oz (sens trigonométrique).
fn azimuthal(p: Vec3) -> Option<Vec3> {
    let rho = (p.x * p.x + p.y * p.y).sqrt();
    (rho > 0.0).then(|| Vec3::new(-p.y / rho, p.x / rho, 0.0))
}

fn assign_velocities(rng: &mut impl Rng, ic: &InitialConditions, g: f64, b: &mut BodySet) {
    match ic.velocity {
        VelocityMode::Rest => {}
        VelocityMode::Random { min, max } => {
            for v in &mut b.velocities {
                *v = unit_vector(rng) * uniform(rng, min, max);
            }
        }
        VelocityMode::CircularFixed { min, max } => {
            for (p, v) in b.positions.iter().zip(&mut b.velocities) {
                if let Some(t) = azimuthal(*p) {
                    *v = t * uniform(rng, min, max);
                }
            }
        }
        VelocityMode::Radial { min, max } => {
            for (p, v) in b.positions.iter().zip(&mut b.velocities) {
                let r = p.length();
                if r > 0.0 {
                    *v = -*p / r * uniform(rng, min, max);
                }
            }
        }
        VelocityMode::CircularOrbital { spread } => {
            // Masse intérieure M(<r) : rang dans le tri par rayon sphérique.
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
            let spread = spread.abs();
            for i in 0..n {
                let p = b.positions[i];
                let r2 = p.length_squared();
                let rho2 = p.x * p.x + p.y * p.y;
                if r2 <= 0.0 {
                    continue;
                }
                let Some(t) = azimuthal(p) else { continue };
                // Composante cylindrique de l'attraction : g_ρ = G·M(<r)·ρ / r³
                // ⇒ v = √(ρ·g_ρ) = √(G·M(<r)·ρ² / r³).
                let v_circ = (g * enclosed[i] * rho2 / (r2 * r2.sqrt())).sqrt();
                let factor = if spread > 0.0 {
                    1.0 + rng.gen_range(-spread..spread)
                } else {
                    1.0
                };
                b.velocities[i] = t * v_circ * factor;
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
        }
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
        // Recentrage : léger débordement possible, borné.
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
            VelocityMode::CircularOrbital { spread: 0.1 },
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
            VelocityMode::CircularOrbital { spread: 0.0 },
        );
        let b = generate_bodies(&c, 2.0);
        // Pour le corps le plus extérieur, M(<r) ≈ N − 1.
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
    }
}
