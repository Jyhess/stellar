//! # stellar-application
//!
//! Use cases of the simulator. This layer orchestrates the domain and
//! declares the **ports** (traits) that the infrastructure implements:
//! - [`ports::SimulationRepository`] and the recording ports for
//!   persistence;
//! - [`stellar_domain::EngineFactory`] (through [`registry::EngineRegistry`])
//!   for the computation engines.
//!
//! It knows neither the graphical interface, nor the file format, nor how
//! the engines are implemented.

pub mod generator;
pub mod headless;
pub mod ports;
pub mod recording;
pub mod registry;
pub mod replay;
pub mod runner;
pub mod service;
pub mod systems;
pub mod trace;

pub use ports::{RepositoryError, SaveData, SaveSummary, SimulationRepository, ViewState};
pub use recording::{
    BodyFate, FrameMeta, Keyframe, RecordingHeader, RecordingInfo, RecordingLink, RecordingReader,
    RecordingRepository, RecordingStatus, RecordingSummary, RecordingWriter,
};
pub use registry::EngineRegistry;
pub use replay::{PlaybackClock, ReplayPlayer};
pub use runner::{Phase, SimulationHandle, Snapshot};
pub use headless::{HeadlessRun, Progress, RecordSettings, RunOutcome, RunTarget};
pub use systems::MultiSystem;
pub use service::{name_from_file, AppError, Continuation, LoadedSimulation, SimulatorService};
pub use trace::{Trace, TraceReference, TraceSet};
