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

/// Initial conditions of a new simulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InitialConditions {
    pub count: usize,
    /// Radius of the cloud (sphere or disc).
    pub radius: f64,
    pub seed: u64,
    pub distribution: Distribution,
    pub velocity: VelocityMode,
}

impl Default for InitialConditions {
    fn default() -> Self {
        Self {
            count: 1_000_000,
            radius: 100.0,
            seed: 42,
            distribution: Distribution::Sphere,
            velocity: VelocityMode::Rest,
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

    /// Characteristic speed of the system, √(G·M/R).
    pub fn characteristic_speed(&self, g: f64) -> f64 {
        (g * self.count as f64 / self.radius.max(1e-12)).sqrt()
    }

    /// Free-fall time of a homogeneous sphere of the same mean density.
    pub fn free_fall_time(&self, g: f64) -> f64 {
        let rho = self.count as f64 / self.volume().max(1e-300);
        (3.0 * std::f64::consts::PI / (32.0 * g * rho)).sqrt()
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
    /// - dt = t_ff / 500 (free-fall time);
    /// - θ = 0.7; cleanup every 100 steps beyond 10 × the cloud radius;
    /// - ejected clusters: link length = mean distance between neighbours, at
    ///   least 10 bodies or 0.1 % of the initial mass.
    pub fn suggested(initial: &InitialConditions, g: f64) -> Self {
        let spacing = initial.mean_spacing();
        Self {
            g,
            dt: initial.free_fall_time(g) / 500.0,
            softening: SOFTENING_PER_SPACING * spacing,
            theta: 0.7,
            base_radius: 0.01 * spacing,
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
