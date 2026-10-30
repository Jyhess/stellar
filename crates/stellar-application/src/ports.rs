//! Output ports: interfaces implemented by the infrastructure.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stellar_domain::{BodyId, SimulationConfig, SimulationState};

use crate::recording::RecordingLink;

/// Camera viewpoint, saved with the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewState {
    pub position: [f64; 3],
    /// Azimuth (radians, around Oz).
    pub yaw: f64,
    /// Elevation (radians).
    pub pitch: f64,
    /// Roll around the line of sight (radians, positive to the right).
    pub roll: f64,
    /// Vertical field of view (radians).
    pub fov_y: f64,
    /// Tracked body, if any.
    pub followed: Option<BodyId>,
    /// Barycenter tracking (if no body is tracked).
    pub barycenter: bool,
    /// Distance to the tracked body or barycenter (orbit mode).
    pub orbit_distance: f64,
}

/// Full content of a save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveData {
    pub config: SimulationConfig,
    pub state: SimulationState,
    pub view: Option<ViewState>,
    /// Suspended recording to resume on load.
    pub recording: Option<RecordingLink>,
    /// Recording running when an F5 save was made: it is continued on load
    /// (later snapshots deleted).
    pub continue_recording: Option<PathBuf>,
}

/// Summary of a save, readable without loading the whole file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveSummary {
    #[serde(skip)]
    pub path: PathBuf,
    pub name: String,
    /// Save date (Unix seconds, UTC).
    pub saved_at_unix: i64,
    pub body_count: usize,
    pub step: u64,
    pub time: f64,
    pub engine: String,
    /// Linked recording (file name), resumed automatically on load.
    pub recording: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("erreur d'entrée/sortie : {0}")]
    Io(String),
    #[error("fichier invalide : {0}")]
    Format(String),
    #[error("version de format non prise en charge : {0}")]
    UnsupportedVersion(u32),
}

/// Persistence of simulations.
pub trait SimulationRepository: Send + Sync {
    /// Saves under a given name; returns the path of the created file.
    fn save(&self, name: &str, data: &SaveData) -> Result<PathBuf, RepositoryError>;

    fn load(&self, path: &Path) -> Result<SaveData, RepositoryError>;

    /// Lists the available saves, most recent first.
    fn list(&self) -> Result<Vec<SaveSummary>, RepositoryError>;

    /// Deletes a save (to the recycle bin if the repository provides it).
    fn delete(&self, path: &Path) -> Result<(), RepositoryError>;

    /// Renames a save (same folder); returns the new path. Refuses to overwrite
    /// an existing save.
    fn rename(&self, path: &Path, name: &str) -> Result<PathBuf, RepositoryError>;

    /// Replaces, in a save, the link to the recording `from` (compared by file
    /// name) with `to`.
    fn relink(&self, path: &Path, from: &Path, to: &Path) -> Result<(), RepositoryError>;
}
