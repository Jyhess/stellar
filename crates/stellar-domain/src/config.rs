use serde::{Deserialize, Serialize};

/// Radial density profile of a disc.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DensityProfile {
    /// Uniform surface density.
    Uniform,
    /// Surface density ∝ exp(−r / (scale·R)), truncated at radius R.
    Exponential { scale: f64 },
}

/// Shape of the initial cloud.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Distribution {
    /// Solid ball of radius R, uniform density.
    Sphere,
    /// Disc of radius R in the (x, y) plane, of thickness `thickness`·R.
    Disk {
        thickness: f64,
        profile: DensityProfile,
    },
}

/// Initial velocity of the particles — mutually exclusive modes.
///
/// Circular motions are around the vertical axis (Oz), counter-clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum VelocityMode {
    /// All particles at rest.
    Rest,
    /// Random isotropic direction, magnitude uniform in [min, max].
    Random { min: f64, max: f64 },
    /// Rotation around Oz, magnitude uniform in [min, max].
    CircularFixed { min: f64, max: f64 },
    /// Rotation around Oz at the orbital equilibrium speed (computed from the
    /// enclosed mass), multiplied by `factor` (1: circular orbits; less:
    /// collapse; more: expansion), then by a factor uniform in
    /// [1 − spread, 1 + spread].
    CircularOrbital { spread: f64, factor: f64 },
    /// Velocity towards the center, magnitude uniform in [min, max].
    Radial { min: f64, max: f64 },
}

/// Mass spectrum of the dust: integer masses from 1 to `max`, drawn from a
/// power law of density ∝ m^`exponent` (sampled on [1, max + 1) and rounded
/// down). `max` = 1: every particle has mass 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DustMasses {
    pub max: u32,
    pub exponent: f64,
}

impl Default for DustMasses {
    fn default() -> Self {
        Self {
            max: 1,
            exponent: -2.0,
        }
    }
}

/// Upper bound of [`DustMasses::max`].
pub const MAX_DUST_MASS: u32 = 10_000;

impl DustMasses {
    fn max_clamped(&self) -> u32 {
        self.max.clamp(1, MAX_DUST_MASS)
    }

    /// Probability of the integer mass `k` (1 ≤ k ≤ max).
    fn probability(&self, k: u32) -> f64 {
        let (lo, hi) = (k as f64, k as f64 + 1.0);
        let top = self.max_clamped() as f64 + 1.0;
        let a = self.exponent;
        if (a + 1.0).abs() < 1e-9 {
            (hi / lo).ln() / top.ln()
        } else {
            let e = a + 1.0;
            (hi.powf(e) - lo.powf(e)) / (top.powf(e) - 1.0)
        }
    }

    /// Expected mass of a particle.
    pub fn mean(&self) -> f64 {
        let max = self.max_clamped();
        if max == 1 {
            return 1.0;
        }
        (1..=max).map(|k| k as f64 * self.probability(k)).sum()
    }

    /// Mass for a uniform draw `u` in [0, 1) (inverse distribution function).
    pub fn sample(&self, u: f64) -> f64 {
        let max = self.max_clamped();
        if max == 1 {
            return 1.0;
        }
        let top = max as f64 + 1.0;
        let a = self.exponent;
        let m = if (a + 1.0).abs() < 1e-9 {
            top.powf(u)
        } else {
            let e = a + 1.0;
            (1.0 + u * (top.powf(e) - 1.0)).powf(1.0 / e)
        };
        m.floor().clamp(1.0, max as f64)
    }
}

/// Massive bodies (protoplanets) on circular orbits in the (x, y) plane, at
/// radii evenly spaced between `inner`·R and `outer`·R, each at a random
/// angle. `count` = 0: none.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Planets {
    pub count: u32,
    /// Mass of each body, as a share of the total mass.
    pub mass_fraction: f64,
    pub inner: f64,
    pub outer: f64,
}

impl Default for Planets {
    fn default() -> Self {
        Self {
            count: 0,
            mass_fraction: 0.01,
            inner: 0.2,
            outer: 0.9,
        }
    }
}

/// Upper bound of [`Planets::count`].
pub const MAX_PLANETS: u32 = 100;
/// Upper bound of the share of the total mass held by the sun and the planets.
pub const MAX_MASSIVE_FRACTION: f64 = 0.999;

/// Initial conditions of a new simulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InitialConditions {
    /// Number of dust particles.
    pub count: usize,
    /// Radius of the cloud (sphere or disc).
    pub radius: f64,
    pub seed: u64,
    pub distribution: Distribution,
    pub velocity: VelocityMode,
    pub dust: DustMasses,
    /// Central sun at rest, as a share of the total mass.
    pub sun: Option<f64>,
    pub planets: Planets,
}

impl Default for InitialConditions {
    fn default() -> Self {
        Self {
            count: 1_000_000,
            radius: 100.0,
            seed: 42,
            distribution: Distribution::Sphere,
            velocity: VelocityMode::Rest,
            dust: DustMasses::default(),
            sun: None,
            planets: Planets::default(),
        }
    }
}

impl InitialConditions {
    /// Volume occupied by the initial cloud.
    pub fn volume(&self) -> f64 {
        let r = self.radius;
        match self.distribution {
            Distribution::Sphere => 4.0 / 3.0 * std::f64::consts::PI * r.powi(3),
            Distribution::Disk { thickness, .. } => {
                std::f64::consts::PI * r * r * (thickness.max(1e-6) * r)
            }
        }
    }

    /// Mean distance between neighbouring particles (≈ (V/N)^(1/3)).
    pub fn mean_spacing(&self) -> f64 {
        (self.volume() / self.count.max(1) as f64).cbrt()
    }

    /// Number of bodies created: dust, sun and planets.
    pub fn body_count(&self) -> usize {
        self.count + usize::from(self.sun.is_some()) + self.planets.count as usize
    }

    /// Share of the total mass held by the sun and the planets.
    pub fn massive_fraction(&self) -> f64 {
        self.sun.unwrap_or(0.0) + self.planets.count as f64 * self.planets.mass_fraction
    }

    /// The sun and the planets leave some mass to the dust (share at most
    /// [`MAX_MASSIVE_FRACTION`]).
    pub fn masses_are_valid(&self) -> bool {
        let f = self.massive_fraction();
        f.is_finite() && (0.0..=MAX_MASSIVE_FRACTION + 1e-12).contains(&f)
    }

    /// Total mass for a given dust mass: the sun and the planets take their
    /// share of it.
    pub fn total_mass_for_dust(&self, dust_mass: f64) -> f64 {
        dust_mass / (1.0 - self.massive_fraction().clamp(0.0, MAX_MASSIVE_FRACTION))
    }

    /// Expected total mass (the exact one depends on the draw of the dust
    /// masses).
    pub fn expected_total_mass(&self) -> f64 {
        self.total_mass_for_dust(self.count as f64 * self.dust.mean())
    }

    /// Characteristic speed of the system, √(G·M/R).
    pub fn characteristic_speed(&self, g: f64) -> f64 {
        (g * self.expected_total_mass() / self.radius.max(1e-12)).sqrt()
    }

    /// Free-fall time of a homogeneous sphere of the same mean density.
    pub fn free_fall_time(&self, g: f64) -> f64 {
        let rho = self.expected_total_mass() / self.volume().max(1e-300);
        (3.0 * std::f64::consts::PI / (32.0 * g * rho)).sqrt()
    }

    /// Orbital period around the sun at the larger of its radius and the
    /// mean spacing (`None` without a sun).
    pub fn inner_orbital_period(&self, g: f64, base_radius: f64) -> Option<f64> {
        let share = self.sun?;
        let sun_mass = share * self.expected_total_mass();
        let r = (base_radius * sun_mass.cbrt()).max(self.mean_spacing());
        Some(std::f64::consts::TAU * (r.powi(3) / (g * sun_mass).max(1e-300)).sqrt())
    }
}

/// Physical and numerical parameters — fixed once the simulation has started.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhysicsParams {
    /// Gravitational constant.
    pub g: f64,
    /// Time step (fixed).
    pub dt: f64,
    /// Plummer softening length ε.
    pub softening: f64,
    /// Barnes-Hut opening criterion θ.
    pub theta: f64,
    /// Radius r₀ of a particle of mass 1; r = r₀·m^(1/3).
    pub base_radius: f64,
    /// Period (in steps) of the cleanup of ejected bodies; 0 = disabled.
    pub cleanup_period: u32,
    /// A body is removed if it is unbound and farther than
    /// `cleanup_distance_factor × reference_radius` from the barycenter.
    pub cleanup_distance_factor: f64,
    /// Reference radius (radius of the initial cloud).
    pub reference_radius: f64,
    /// Ejected clusters (see [`crate::ejection`]): link length of the
    /// "friends of friends" groups.
    pub cluster_link_length: f64,
    /// A bound cluster of at least this many bodies is kept (0 = criterion
    /// ignored).
    pub cluster_min_bodies: u32,
    /// … or of at least this share of the initial mass (0 = criterion ignored).
    pub cluster_min_mass: f64,
}

/// Suggested ε, relative to the mean distance between neighbours.
pub const SOFTENING_PER_SPACING: f64 = 0.3;

impl PhysicsParams {
    /// Default values derived from the initial conditions:
    /// - ε = 0.3 × mean distance between neighbours;
    /// - r₀ = 1 % of that distance;
    /// - dt = t_ff / 500 (free-fall time), and at most 1/200 of the orbital
    ///   period around the sun at its surface or at the mean spacing;
    /// - θ = 0.7; cleanup every 100 steps beyond 10 × the cloud radius;
    /// - ejected clusters: link length = mean distance between neighbours, at
    ///   least 10 bodies or 0.1 % of the initial mass.
    pub fn suggested(initial: &InitialConditions, g: f64) -> Self {
        let spacing = initial.mean_spacing();
        let base_radius = 0.01 * spacing;
        let mut dt = initial.free_fall_time(g) / 500.0;
        if let Some(period) = initial.inner_orbital_period(g, base_radius) {
            dt = dt.min(period / 200.0);
        }
        Self {
            g,
            dt,
            softening: SOFTENING_PER_SPACING * spacing,
            theta: 0.7,
            base_radius,
            cleanup_period: 100,
            cleanup_distance_factor: 10.0,
            reference_radius: initial.radius,
            cluster_link_length: spacing,
            cluster_min_bodies: 10,
            cluster_min_mass: 1e-3,
        }
    }
}

impl Default for PhysicsParams {
    fn default() -> Self {
        Self::suggested(&InitialConditions::default(), 1.0)
    }
}

/// Complete configuration of a simulation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationConfig {
    pub initial: InitialConditions,
    pub physics: PhysicsParams,
    /// Identifier of the computation engine (see [`crate::EngineDescriptor`]).
    pub engine: String,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            initial: InitialConditions::default(),
            physics: PhysicsParams::default(),
            engine: "cpu-barnes-hut".to_owned(),
        }
    }
}
