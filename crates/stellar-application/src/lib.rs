//! # stellar-application
//!
//! Cas d'usage du simulateur. Cette couche orchestre le domaine et déclare
//! les **ports** (traits) que l'infrastructure implémente :
//! - [`ports::SimulationRepository`] pour la persistance ;
//! - [`stellar_domain::EngineFactory`] (via [`registry::EngineRegistry`]) pour
//!   les moteurs de calcul.
//!
//! Elle ne connaît ni l'interface graphique, ni le format des fichiers, ni
//! l'implémentation des moteurs.

pub mod generator;
pub mod ports;
pub mod registry;
pub mod runner;
pub mod service;

pub use ports::{RepositoryError, SaveData, SaveSummary, SimulationRepository, ViewState};
pub use registry::EngineRegistry;
pub use runner::{Phase, SimulationHandle, Snapshot};
pub use service::{AppError, SimulatorService};
