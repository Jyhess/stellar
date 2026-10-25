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
}
