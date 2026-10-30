use serde::{Deserialize, Serialize};

use crate::BodySet;

/// Instantaneous state of a simulation: enough to resume it exactly.
///
/// Velocities are synchronized with positions (whole step).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimulationState {
    pub bodies: BodySet,
    /// Number of steps since creation.
    pub step: u64,
    /// Simulated time.
    pub time: f64,
    /// Initial total energy (reference for monitoring the drift).
    pub initial_energy: Option<f64>,
    /// Initial total mass (reference for the ejected mass).
    pub initial_mass: Option<f64>,
    /// Subsystems (see [`crate::systems`]): ranges of the bodies, grouped by
    /// subsystem. Empty: a single system.
    pub systems: Vec<crate::SystemSpan>,
}
