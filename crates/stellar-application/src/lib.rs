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
pub mod recording;
pub mod registry;
pub mod replay;
pub mod runner;
pub mod service;
pub mod trace;

pub use ports::{RepositoryError, SaveData, SaveSummary, SimulationRepository, ViewState};
pub use recording::{
    BodyFate, FrameMeta, Keyframe, RecordingHeader, RecordingInfo, RecordingLink, RecordingReader,
    RecordingRepository, RecordingStatus, RecordingSummary, RecordingWriter,
};
pub use registry::EngineRegistry;
pub use replay::{PlaybackClock, ReplayPlayer};
pub use runner::{Phase, SimulationHandle, Snapshot};
pub use service::{AppError, LoadedSimulation, SimulatorService};
pub use trace::{Trace, TraceSet};
