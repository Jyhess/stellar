//! Ports de sortie : interfaces implémentées par l'infrastructure.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stellar_domain::{BodyId, SimulationConfig, SimulationState};

use crate::recording::RecordingLink;

/// Point de vue de la caméra, enregistré avec la simulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewState {
    pub position: [f64; 3],
    /// Azimut (radians, autour de Oz).
    pub yaw: f64,
    /// Élévation (radians).
    pub pitch: f64,
    /// Bascule sur l'axe de visée (radians, positive vers la droite).
    pub roll: f64,
    /// Champ de vision vertical (radians).
    pub fov_y: f64,
    /// Corps suivi, le cas échéant.
    pub followed: Option<BodyId>,
    /// Suivi du barycentre (si aucun corps n'est suivi).
    pub barycenter: bool,
    /// Distance au corps ou au barycentre suivi (mode orbite).
    pub orbit_distance: f64,
}

/// Contenu complet d'une sauvegarde.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveData {
    pub config: SimulationConfig,
    pub state: SimulationState,
    pub view: Option<ViewState>,
    /// Enregistrement suspendu à reprendre au chargement.
    pub recording: Option<RecordingLink>,
    /// Enregistrement en cours au moment d'une sauvegarde F5 : il est
    /// poursuivi au chargement (instantanés postérieurs supprimés).
    pub continue_recording: Option<PathBuf>,
}

/// Résumé d'une sauvegarde, lisible sans charger tout le fichier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveSummary {
    #[serde(skip)]
    pub path: PathBuf,
    pub name: String,
    /// Date d'enregistrement (secondes Unix, UTC).
    pub saved_at_unix: i64,
    pub body_count: usize,
    pub step: u64,
    pub time: f64,
    pub engine: String,
    /// Enregistrement lié (nom du fichier), repris automatiquement au chargement.
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

/// Persistance des simulations.
pub trait SimulationRepository: Send + Sync {
    /// Enregistre sous un nom donné ; renvoie le chemin du fichier créé.
    fn save(&self, name: &str, data: &SaveData) -> Result<PathBuf, RepositoryError>;

    fn load(&self, path: &Path) -> Result<SaveData, RepositoryError>;

    /// Liste les sauvegardes disponibles, la plus récente d'abord.
    fn list(&self) -> Result<Vec<SaveSummary>, RepositoryError>;
}
