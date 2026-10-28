//! Relecture des formes antérieures des fichiers.
//!
//! bincode n'étant pas auto-descriptif, chaque version garde ici la copie
//! des structures qu'elle sérialisait, convertie vers la forme courante.
//!
//! - Sauvegardes `.stellar` version 3 et enregistrements `.stellarec`
//!   version 1 : paramètres physiques sans les seuils des amas éjectés
//!   (complétés par les valeurs suggérées).
//! - Sauvegardes versions 3 et 4 : état sans sous-systèmes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use stellar_application::{RecordingHeader, RecordingLink, SaveData, ViewState};
use stellar_domain::{
    BodySet, InitialConditions, PhysicsParams, SimulationConfig, SimulationState,
};

/// Paramètres physiques avant les amas éjectés.
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
    pub initial: InitialConditions,
    pub physics: PhysicsParamsV3,
    pub engine: String,
}

impl From<SimulationConfigV3> for SimulationConfig {
    fn from(c: SimulationConfigV3) -> Self {
        let p = c.physics;
        let suggested = PhysicsParams::suggested(&c.initial, p.g);
        Self {
            initial: c.initial,
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

/// État d'une simulation avant les sous-systèmes.
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

/// Contenu d'une sauvegarde version 4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveDataV4 {
    pub config: SimulationConfig,
    pub state: SimulationStateV4,
    pub view: Option<ViewState>,
    pub recording: Option<RecordingLink>,
    pub continue_recording: Option<PathBuf>,
}

impl From<SaveDataV4> for SaveData {
    fn from(d: SaveDataV4) -> Self {
        Self {
            config: d.config,
            state: d.state.into(),
            view: d.view,
            recording: d.recording,
            continue_recording: d.continue_recording,
        }
    }
}

/// Contenu d'une sauvegarde version 3.
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

/// En-tête d'un enregistrement version 1.
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
