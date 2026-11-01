//! Reading of the former shapes of the files.
//!
//! bincode is not self-describing, so each version keeps here a copy of
//! the structures it serialized, converted to the current shape.
//!
//! - `.stellar` saves version 3 and `.stellarec` recordings version 1:
//!   physical parameters without the thresholds of ejected clusters (filled
//!   with the suggested values).
//! - Saves versions 3 and 4: state without subsystems.
//! - Saves versions 3 to 5 and recordings versions 1 to 3: orbital velocity
//!   without factor (factor 1).
//! - Saves versions 3 to 6 and recordings versions 1 to 4: dust of mass 1,
//!   no sun, no planets.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use stellar_application::{RecordingHeader, RecordingLink, SaveData, ViewState};
use stellar_domain::{
    BodySet, Distribution, DustMasses, InitialConditions, PhysicsParams, Planets,
    SimulationConfig, SimulationState, VelocityMode,
};

/// Initial velocities before the orbital velocity factor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum VelocityModeV5 {
    Rest,
    Random { min: f64, max: f64 },
    CircularFixed { min: f64, max: f64 },
    CircularOrbital { spread: f64 },
    Radial { min: f64, max: f64 },
}

impl From<VelocityModeV5> for VelocityMode {
    fn from(v: VelocityModeV5) -> Self {
        match v {
            VelocityModeV5::Rest => Self::Rest,
            VelocityModeV5::Random { min, max } => Self::Random { min, max },
            VelocityModeV5::CircularFixed { min, max } => Self::CircularFixed { min, max },
            VelocityModeV5::CircularOrbital { spread } => {
                Self::CircularOrbital { spread, factor: 1.0 }
            }
            VelocityModeV5::Radial { min, max } => Self::Radial { min, max },
        }
    }
}

impl From<VelocityMode> for VelocityModeV5 {
    fn from(v: VelocityMode) -> Self {
        match v {
            VelocityMode::Rest => Self::Rest,
            VelocityMode::Random { min, max } => Self::Random { min, max },
            VelocityMode::CircularFixed { min, max } => Self::CircularFixed { min, max },
            VelocityMode::CircularOrbital { spread, .. } => Self::CircularOrbital { spread },
            VelocityMode::Radial { min, max } => Self::Radial { min, max },
        }
    }
}

/// Initial conditions before the orbital velocity factor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InitialConditionsV5 {
    pub count: usize,
    pub radius: f64,
    pub seed: u64,
    pub distribution: Distribution,
    pub velocity: VelocityModeV5,
}

impl From<InitialConditionsV5> for InitialConditions {
    fn from(c: InitialConditionsV5) -> Self {
        InitialConditionsV6 {
            count: c.count,
            radius: c.radius,
            seed: c.seed,
            distribution: c.distribution,
            velocity: c.velocity.into(),
        }
        .into()
    }
}

impl From<InitialConditions> for InitialConditionsV5 {
    fn from(c: InitialConditions) -> Self {
        Self {
            count: c.count,
            radius: c.radius,
            seed: c.seed,
            distribution: c.distribution,
            velocity: c.velocity.into(),
        }
    }
}

/// Initial conditions before the dust mass spectrum, the sun and the planets.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InitialConditionsV6 {
    pub count: usize,
    pub radius: f64,
    pub seed: u64,
    pub distribution: Distribution,
    pub velocity: VelocityMode,
}

impl From<InitialConditionsV6> for InitialConditions {
    fn from(c: InitialConditionsV6) -> Self {
        Self {
            count: c.count,
            radius: c.radius,
            seed: c.seed,
            distribution: c.distribution,
            velocity: c.velocity,
            dust: DustMasses::default(),
            sun: None,
            planets: Planets::default(),
        }
    }
}

impl From<InitialConditions> for InitialConditionsV6 {
    fn from(c: InitialConditions) -> Self {
        Self {
            count: c.count,
            radius: c.radius,
            seed: c.seed,
            distribution: c.distribution,
            velocity: c.velocity,
        }
    }
}

/// Configuration of saves version 6 and of recordings version 4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationConfigV6 {
    pub initial: InitialConditionsV6,
    pub physics: PhysicsParams,
    pub engine: String,
}

impl From<SimulationConfigV6> for SimulationConfig {
    fn from(c: SimulationConfigV6) -> Self {
        Self {
            initial: c.initial.into(),
            physics: c.physics,
            engine: c.engine,
        }
    }
}

/// Configuration of saves versions 4 and 5 and of recordings versions 2
/// and 3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationConfigV5 {
    pub initial: InitialConditionsV5,
    pub physics: PhysicsParams,
    pub engine: String,
}

impl From<SimulationConfigV5> for SimulationConfig {
    fn from(c: SimulationConfigV5) -> Self {
        Self {
            initial: c.initial.into(),
            physics: c.physics,
            engine: c.engine,
        }
    }
}

/// Physical parameters before ejected clusters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhysicsParamsV3 {
    pub g: f64,
    pub dt: f64,
    pub softening: f64,
    pub theta: f64,
    pub base_radius: f64,
    pub cleanup_period: u32,
    pub cleanup_distance_factor: f64,
    pub reference_radius: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationConfigV3 {
    pub initial: InitialConditionsV5,
    pub physics: PhysicsParamsV3,
    pub engine: String,
}

impl From<SimulationConfigV3> for SimulationConfig {
    fn from(c: SimulationConfigV3) -> Self {
        let p = c.physics;
        let initial = c.initial.into();
        let suggested = PhysicsParams::suggested(&initial, p.g);
        Self {
            initial,
            physics: PhysicsParams {
                g: p.g,
                dt: p.dt,
                softening: p.softening,
                theta: p.theta,
                base_radius: p.base_radius,
                cleanup_period: p.cleanup_period,
                cleanup_distance_factor: p.cleanup_distance_factor,
                reference_radius: p.reference_radius,
                cluster_link_length: suggested.cluster_link_length,
                cluster_min_bodies: suggested.cluster_min_bodies,
                cluster_min_mass: suggested.cluster_min_mass,
            },
            engine: c.engine,
        }
    }
}

/// State of a simulation before subsystems.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationStateV4 {
    pub bodies: BodySet,
    pub step: u64,
    pub time: f64,
    pub initial_energy: Option<f64>,
    pub initial_mass: Option<f64>,
}

impl From<SimulationStateV4> for SimulationState {
    fn from(s: SimulationStateV4) -> Self {
        Self {
            bodies: s.bodies,
            step: s.step,
            time: s.time,
            initial_energy: s.initial_energy,
            initial_mass: s.initial_mass,
            systems: Vec::new(),
        }
    }
}

/// Content of a save version 6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveDataV6 {
    pub config: SimulationConfigV6,
    pub state: SimulationState,
    pub view: Option<ViewState>,
    pub recording: Option<RecordingLink>,
    pub continue_recording: Option<PathBuf>,
}

impl From<SaveDataV6> for SaveData {
    fn from(d: SaveDataV6) -> Self {
        Self {
            config: d.config.into(),
            state: d.state,
            view: d.view,
            recording: d.recording,
            continue_recording: d.continue_recording,
        }
    }
}

/// Content of a save version 5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveDataV5 {
    pub config: SimulationConfigV5,
    pub state: SimulationState,
    pub view: Option<ViewState>,
    pub recording: Option<RecordingLink>,
    pub continue_recording: Option<PathBuf>,
}

impl From<SaveDataV5> for SaveData {
    fn from(d: SaveDataV5) -> Self {
        Self {
            config: d.config.into(),
            state: d.state,
            view: d.view,
            recording: d.recording,
            continue_recording: d.continue_recording,
        }
    }
}

/// Content of a save version 4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveDataV4 {
    pub config: SimulationConfigV5,
    pub state: SimulationStateV4,
    pub view: Option<ViewState>,
    pub recording: Option<RecordingLink>,
    pub continue_recording: Option<PathBuf>,
}

impl From<SaveDataV4> for SaveData {
    fn from(d: SaveDataV4) -> Self {
        Self {
            config: d.config.into(),
            state: d.state.into(),
            view: d.view,
            recording: d.recording,
            continue_recording: d.continue_recording,
        }
    }
}

/// Content of a save version 3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveDataV3 {
    pub config: SimulationConfigV3,
    pub state: SimulationStateV4,
    pub view: Option<ViewState>,
    pub recording: Option<RecordingLink>,
    pub continue_recording: Option<PathBuf>,
}

impl From<SaveDataV3> for SaveData {
    fn from(d: SaveDataV3) -> Self {
        Self {
            config: d.config.into(),
            state: d.state.into(),
            view: d.view,
            recording: d.recording,
            continue_recording: d.continue_recording,
        }
    }
}

/// Header of a recording version 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingHeaderV1 {
    pub config: SimulationConfigV3,
    pub interval: u32,
    pub start_step: u64,
    pub created_at_unix: i64,
    pub dust_budget: Option<u64>,
}

impl From<RecordingHeaderV1> for RecordingHeader {
    fn from(h: RecordingHeaderV1) -> Self {
        Self {
            config: h.config.into(),
            interval: h.interval,
            start_step: h.start_step,
            created_at_unix: h.created_at_unix,
            dust_budget: h.dust_budget,
        }
    }
}

/// Header of a recording versions 2 and 3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingHeaderV3 {
    pub config: SimulationConfigV5,
    pub interval: u32,
    pub start_step: u64,
    pub created_at_unix: i64,
    pub dust_budget: Option<u64>,
}

impl From<RecordingHeaderV3> for RecordingHeader {
    fn from(h: RecordingHeaderV3) -> Self {
        Self {
            config: h.config.into(),
            interval: h.interval,
            start_step: h.start_step,
            created_at_unix: h.created_at_unix,
            dust_budget: h.dust_budget,
        }
    }
}

/// Header of a recording version 4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingHeaderV4 {
    pub config: SimulationConfigV6,
    pub interval: u32,
    pub start_step: u64,
    pub created_at_unix: i64,
    pub dust_budget: Option<u64>,
}

impl From<RecordingHeaderV4> for RecordingHeader {
    fn from(h: RecordingHeaderV4) -> Self {
        Self {
            config: h.config.into(),
            interval: h.interval,
            start_step: h.start_step,
            created_at_unix: h.created_at_unix,
            dust_budget: h.dust_budget,
        }
    }
}
