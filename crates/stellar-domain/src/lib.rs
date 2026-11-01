//! # stellar-domain
//!
//! Core of the simulator: entities (bodies, state, parameters), physical
//! rules (radius, merger, mass ranking, cleanup of ejected bodies, field
//! between subsystems) and the **abstraction of the computation engine**
//! ([`PhysicsEngine`]).
//!
//! This layer depends on no infrastructure (no GPU, files or threads): only
//! `glam` (vector algebra) and `serde` (data description) are allowed.

pub mod body;
pub mod config;
pub mod engine;
pub mod ejection;
pub mod physics;
pub mod ranking;
pub mod state;
pub mod systems;

pub use body::{BodyId, BodySet};
pub use config::{
    DensityProfile, Distribution, DustMasses, InitialConditions, PhysicsParams, Planets,
    SimulationConfig, VelocityMode, MAX_DUST_MASS, MAX_MASSIVE_FRACTION, MAX_PLANETS,
};
pub use ejection::EjectedCluster;
pub use engine::{Diagnostics, EngineDescriptor, EngineError, EngineFactory, PhysicsEngine, StepReport};
pub use state::SimulationState;
pub use systems::{ExternalField, Moments, SystemSpan, SystemSummary, SystemsView};

/// Double-precision 3D vector used for every physical quantity.
pub type Vec3 = glam::DVec3;
