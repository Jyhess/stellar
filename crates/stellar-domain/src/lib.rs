//! # stellar-domain
//!
//! Cœur métier du simulateur : entités (corps, état, paramètres), règles
//! physiques élémentaires (rayon, fusion, classement par masse) et
//! **abstraction du moteur de calcul** ([`PhysicsEngine`]).
//!
//! Cette couche ne dépend d'aucune infrastructure (ni GPU, ni fichiers, ni
//! threads) : seules `glam` (algèbre vectorielle) et `serde` (description des
//! données) y sont admises.

pub mod body;
pub mod config;
pub mod engine;
pub mod physics;
pub mod ranking;
pub mod state;

pub use body::{BodyId, BodySet};
pub use config::{
    DensityProfile, Distribution, InitialConditions, PhysicsParams, SimulationConfig,
    VelocityMode,
};
pub use engine::{Diagnostics, EngineDescriptor, EngineFactory, PhysicsEngine, StepReport};
pub use state::SimulationState;

/// Vecteur 3D double précision utilisé pour toute grandeur physique.
pub type Vec3 = glam::DVec3;
