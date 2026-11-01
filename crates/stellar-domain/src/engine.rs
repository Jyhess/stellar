//! Abstraction of the computation engine.
//!
//! The rest of the application knows only these traits: a new engine is
//! added by implementing [`EngineFactory`] and [`PhysicsEngine`], then
//! registering it with the application's registry; it can then be selected
//! in the parameters.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use crate::{
    BodyId, BodySet, EjectedCluster, ExternalField, Moments, PhysicsParams, SimulationState,
    SystemSummary, Vec3,
};

/// Description of an engine, shown in the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineDescriptor {
    /// Stable identifier, stored in saves.
    pub id: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    pub description: &'static str,
}

/// Report of a simulation step.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepReport {
    /// Mergers that happened: (absorbed identifier, identifier of the resulting body).
    pub merges: Vec<(BodyId, BodyId)>,
    /// Bodies removed by the cleanup of ejected bodies.
    pub removed: Vec<BodyId>,
    /// The same bodies, as they were when removed (same identifiers, same
    /// order): a composite system can hand them over to another subsystem.
    pub removed_bodies: BodySet,
    /// Kept ejected clusters, described at each cleanup (`None` outside
    /// cleanup steps).
    pub clusters: Option<Vec<EjectedCluster>>,
}

/// Energies computed by the engine at the end of a step.
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

/// Computation engine: advances a state by a fixed time step.
///
/// An engine is driven from a single simulation thread; it may parallelize
/// freely internally.
pub trait PhysicsEngine: Send {
    /// Advances by one time step `dt`, mergers and periodic cleanup included.
    fn step(&mut self) -> StepReport;

    /// Full copy of the current state (positions and velocities synchronized).
    fn state(&self) -> SimulationState;

    /// Copies the current state into `out`, reusing its allocations.
    fn write_state(&self, out: &mut SimulationState) {
        *out = self.state();
    }

    fn body_count(&self) -> usize;

    /// Current positions of the requested bodies (`None` if absent). Called at
    /// every step for traces: engines benefit from specializing it.
    fn positions_of(&self, ids: &[BodyId]) -> Vec<Option<Vec3>> {
        let s = self.state();
        ids.iter()
            .map(|&id| s.bodies.index_of(id).map(|i| s.bodies.positions[i]))
            .collect()
    }

    /// Center of mass of the system (`None` if it is empty). Called at every
    /// step while traces are active.
    fn center_of_mass(&self) -> Option<Vec3> {
        self.state().bodies.center_of_mass()
    }

    /// Energies at the last computed step.
    fn diagnostics(&self) -> Diagnostics;

    /// Direct access to the engine's data for display, without copy (for
    /// example the buffers of a graphics card shared with the interface). The
    /// concrete type belongs to the engine: the presentation layer recognizes it
    /// by `downcast`. `None` by default: display then goes through copies of
    /// the state ([`Self::write_state`]).
    fn live_view(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }

    /// Mass, barycenter and barycenter velocity at the last step.
    fn moments(&self) -> Moments {
        let b = self.state().bodies;
        let mass = b.total_mass();
        Moments {
            mass,
            center: b.center_of_mass().unwrap_or_default(),
            velocity: if mass > 0.0 { b.momentum() / mass } else { Vec3::ZERO },
        }
    }

    /// External field valid at the end of the next step (see
    /// [`crate::systems`]); the step's first half kick uses the field set at
    /// the previous step (or this one, the first time). Returns `false` if the
    /// engine cannot take it into account.
    fn set_external_field(&mut self, field: ExternalField) -> bool {
        let _ = field;
        false
    }

    /// Adds bodies (synchronized velocities; their own acceleration is known
    /// only at the next step). Returns `false` if the engine does not allow it,
    /// or no longer does (capacity reached): the caller then rebuilds it.
    fn insert(&mut self, bodies: &BodySet) -> bool {
        let _ = bodies;
        false
    }

    /// Subsystems computed separately (empty for a simple engine).
    fn systems(&self) -> Vec<SystemSummary> {
        Vec::new()
    }
}

/// Engine factory: the extension point for new engines.
pub trait EngineFactory: Send + Sync {
    fn descriptor(&self) -> EngineDescriptor;

    /// Maximum number of bodies this engine can handle on this machine
    /// (`None`: only main memory limits it).
    fn max_bodies(&self) -> Option<usize> {
        None
    }

    /// Builds an engine from a state and parameters. Fails if the required
    /// hardware is missing or insufficient.
    fn create(
        &self,
        state: SimulationState,
        params: PhysicsParams,
    ) -> Result<Box<dyn PhysicsEngine>, EngineError>;
}

/// Reason why an engine cannot be built. `Display` gives an English message
/// (logs); the interface translates each variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// No compatible graphics card was found.
    NoDevice,
    /// The graphics card does not support compute shaders.
    ComputeUnsupported,
    /// The graphics card could not be opened (driver message).
    DeviceOpen(String),
    /// More bodies than the engine can handle.
    TooManyBodies { requested: usize, limit: usize },
    /// Body identifiers too large for the engine.
    IdsTooLarge,
    /// A buffer would exceed the device's limit.
    BufferTooLarge { device: String, buffer_mb: u64, limit_mb: u64 },
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no compatible graphics card was found"),
            Self::ComputeUnsupported => write!(f, "the graphics card does not support compute shaders"),
            Self::DeviceOpen(e) => write!(f, "cannot open the graphics card: {e}"),
            Self::TooManyBodies { requested, limit } => {
                write!(f, "the engine is limited to {limit} bodies ({requested} requested)")
            }
            Self::IdsTooLarge => write!(f, "body identifiers too large for the engine"),
            Self::BufferTooLarge { device, buffer_mb, limit_mb } => write!(
                f,
                "too many bodies for this graphics card ({device}: buffer of {buffer_mb} MB, limit {limit_mb} MB)"
            ),
        }
    }
}

impl std::error::Error for EngineError {}
