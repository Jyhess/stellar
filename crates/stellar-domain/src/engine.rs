//! Abstraction du moteur de calcul.
//!
//! Le reste de l'application ne connaît que ces traits : un nouveau moteur
//! (par exemple sur GPU) s'ajoute en implémentant [`EngineFactory`] et
//! [`PhysicsEngine`], puis en l'enregistrant auprès du registre de
//! l'application ; il devient alors sélectionnable dans les paramètres.

use std::any::Any;
use std::sync::Arc;

use crate::{
    BodyId, BodySet, EjectedCluster, ExternalField, Moments, PhysicsParams, SimulationState,
    SystemSummary, Vec3,
};

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
    /// Les mêmes corps, tels qu'au moment de leur élimination (mêmes
    /// identifiants, même ordre) : un système composite peut les confier à
    /// un autre sous-système.
    pub removed_bodies: BodySet,
    /// Amas éjectés conservés, décrits à chaque nettoyage (`None` hors
    /// nettoyage).
    pub clusters: Option<Vec<EjectedCluster>>,
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

    /// Positions courantes des corps demandés (`None` si absent). Appelé à
    /// chaque pas pour les traces : les moteurs gagnent à le spécialiser.
    fn positions_of(&self, ids: &[BodyId]) -> Vec<Option<Vec3>> {
        let s = self.state();
        ids.iter()
            .map(|&id| s.bodies.index_of(id).map(|i| s.bodies.positions[i]))
            .collect()
    }

    /// Centre de masse du système (`None` s'il est vide). Appelé à chaque
    /// pas quand les traces sont actives.
    fn center_of_mass(&self) -> Option<Vec3> {
        self.state().bodies.center_of_mass()
    }

    /// Énergies au dernier pas calculé.
    fn diagnostics(&self) -> Diagnostics;

    /// Accès direct aux données du moteur pour l'affichage, sans copie (par
    /// exemple les tampons d'une carte graphique partagée avec l'interface).
    /// Le type concret est propre au moteur : la couche de présentation le
    /// reconnaît par conversion (`downcast`). `None` par défaut : l'affichage
    /// passe alors par les copies de l'état ([`Self::write_state`]).
    fn live_view(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }

    /// Masse, barycentre et vitesse du barycentre au dernier pas.
    fn moments(&self) -> Moments {
        let b = self.state().bodies;
        let mass = b.total_mass();
        Moments {
            mass,
            center: b.center_of_mass().unwrap_or_default(),
            velocity: if mass > 0.0 { b.momentum() / mass } else { Vec3::ZERO },
        }
    }

    /// Champ extérieur valable à la fin du prochain pas (voir
    /// [`crate::systems`]) ; le premier demi-kick du pas utilise le champ
    /// fixé au pas précédent (ou celui-ci, la première fois). Renvoie
    /// `false` si le moteur ne sait pas en tenir compte.
    fn set_external_field(&mut self, field: ExternalField) -> bool {
        let _ = field;
        false
    }

    /// Ajoute des corps (vitesses synchronisées ; leur accélération propre
    /// n'est connue qu'au pas suivant). Renvoie `false` si le moteur ne le
    /// permet pas, ou plus (capacité atteinte) : l'appelant le reconstruit.
    fn insert(&mut self, bodies: &BodySet) -> bool {
        let _ = bodies;
        false
    }

    /// Sous-systèmes calculés séparément (vide pour un moteur simple).
    fn systems(&self) -> Vec<SystemSummary> {
        Vec::new()
    }
}

/// Fabrique de moteurs : point d'extension pour de futurs moteurs.
pub trait EngineFactory: Send + Sync {
    fn descriptor(&self) -> EngineDescriptor;

    /// Nombre maximal de corps que ce moteur peut traiter sur cette machine
    /// (`None` : seule la mémoire centrale le limite).
    fn max_bodies(&self) -> Option<usize> {
        None
    }

    /// Construit un moteur à partir d'un état et de paramètres. Échoue si le
    /// matériel requis est absent ou insuffisant (message lisible).
    fn create(
        &self,
        state: SimulationState,
        params: PhysicsParams,
    ) -> Result<Box<dyn PhysicsEngine>, String>;
}
