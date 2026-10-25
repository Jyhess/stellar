//! Abstraction du moteur de calcul.
//!
//! Le reste de l'application ne connaît que ces traits : un nouveau moteur
//! (par exemple sur GPU) s'ajoute en implémentant [`EngineFactory`] et
//! [`PhysicsEngine`], puis en l'enregistrant auprès du registre de
//! l'application ; il devient alors sélectionnable dans les paramètres.

use crate::{BodyId, PhysicsParams, SimulationState};

/// Description d'un moteur, affichée dans l'interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineDescriptor {
    /// Identifiant stable, enregistré dans les sauvegardes.
    pub id: &'static str,
    /// Libellé lisible.
    pub label: &'static str,
    pub description: &'static str,
}

/// Compte rendu d'un pas de simulation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepReport {
    /// Fusions survenues : (identifiant absorbé, identifiant du corps résultant).
    pub merges: Vec<(BodyId, BodyId)>,
    /// Corps éliminés par le nettoyage des éjectés.
    pub removed: Vec<BodyId>,
}

/// Grandeurs globales calculées par le moteur à l'issue d'un pas.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Diagnostics {
    pub kinetic_energy: f64,
    pub potential_energy: f64,
}

impl Diagnostics {
    pub fn total_energy(&self) -> f64 {
        self.kinetic_energy + self.potential_energy
    }
}

/// Moteur de calcul : fait évoluer un état d'un pas de temps fixe.
///
/// Un moteur est piloté depuis un unique thread de simulation ; il peut
/// paralléliser librement en interne.
pub trait PhysicsEngine: Send {
    /// Avance d'un pas de temps `dt`, fusions et nettoyage périodique inclus.
    fn step(&mut self) -> StepReport;

    /// Copie complète de l'état courant (positions et vitesses synchronisées).
    fn state(&self) -> SimulationState;

    /// Copie l'état courant dans `out`, en réutilisant ses allocations.
    fn write_state(&self, out: &mut SimulationState) {
        *out = self.state();
    }

    fn body_count(&self) -> usize;

    /// Énergies au dernier pas calculé.
    fn diagnostics(&self) -> Diagnostics;
}

/// Fabrique de moteurs : point d'extension pour de futurs moteurs.
pub trait EngineFactory: Send + Sync {
    fn descriptor(&self) -> EngineDescriptor;

    /// Construit un moteur à partir d'un état et de paramètres.
    fn create(&self, state: SimulationState, params: PhysicsParams) -> Box<dyn PhysicsEngine>;
}
