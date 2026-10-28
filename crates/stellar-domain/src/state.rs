use serde::{Deserialize, Serialize};

use crate::BodySet;

/// État instantané d'une simulation : suffisant pour la reprendre exactement.
///
/// Les vitesses sont synchronisées avec les positions (pas entier).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimulationState {
    pub bodies: BodySet,
    /// Nombre de pas effectués depuis la création.
    pub step: u64,
    /// Temps simulé.
    pub time: f64,
    /// Énergie totale initiale (référence pour le suivi de la dérive).
    pub initial_energy: Option<f64>,
    /// Masse totale initiale (référence pour la masse éjectée).
    pub initial_mass: Option<f64>,
    /// Sous-systèmes (voir [`crate::systems`]) : plages des corps, rangés
    /// par sous-système. Vide : un seul système.
    pub systems: Vec<crate::SystemSpan>,
}
