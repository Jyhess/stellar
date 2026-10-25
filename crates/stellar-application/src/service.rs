//! Façade des cas d'usage, utilisée par la couche de présentation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use stellar_domain::{EngineDescriptor, SimulationConfig};

use crate::generator::generate_state;
use crate::ports::{RepositoryError, SaveData, SaveSummary, SimulationRepository, ViewState};
use crate::registry::EngineRegistry;
use crate::runner::{Phase, SimulationHandle, Snapshot};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("moteur de calcul inconnu : « {0} »")]
    UnknownEngine(String),
    #[error("la simulation n'est pas encore prête")]
    NotReady,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

/// Point d'entrée des cas d'usage : créer, charger, sauvegarder, lister.
#[derive(Clone)]
pub struct SimulatorService {
    registry: Arc<EngineRegistry>,
    repository: Arc<dyn SimulationRepository>,
}

impl SimulatorService {
    pub fn new(registry: EngineRegistry, repository: Arc<dyn SimulationRepository>) -> Self {
        Self { registry: Arc::new(registry), repository }
    }

    /// Moteurs de calcul disponibles.
    pub fn engines(&self) -> Vec<EngineDescriptor> {
        self.registry.descriptors()
    }

    pub fn default_engine(&self) -> Option<&'static str> {
        self.registry.default_id()
    }

    /// Crée et démarre une nouvelle simulation. La génération du nuage a lieu
    /// dans le thread de simulation.
    pub fn new_simulation(&self, config: SimulationConfig) -> Result<SimulationHandle, AppError> {
        let factory = self
            .registry
            .get(&config.engine)
            .ok_or_else(|| AppError::UnknownEngine(config.engine.clone()))?;
        let initial = config.initial;
        let g = config.physics.g;
        Ok(SimulationHandle::start(
            config,
            factory,
            move || generate_state(&initial, g),
            false,
        ))
    }

    /// Charge une sauvegarde et la redémarre en pause.
    pub fn load_simulation(
        &self,
        path: &Path,
    ) -> Result<(SimulationHandle, Option<ViewState>), AppError> {
        let data = self.repository.load(path)?;
        let factory = self
            .registry
            .get(&data.config.engine)
            .ok_or_else(|| AppError::UnknownEngine(data.config.engine.clone()))?;
        let SaveData { config, state, view } = data;
        let handle = SimulationHandle::start(config, factory, move || state, true);
        Ok((handle, view))
    }

    /// Sauvegarde un instantané. Peut être appelé depuis un thread
    /// d'arrière-plan : l'instantané est immuable et partagé.
    pub fn save(
        &self,
        name: &str,
        config: &SimulationConfig,
        snapshot: &Snapshot,
        view: Option<ViewState>,
    ) -> Result<PathBuf, AppError> {
        if snapshot.phase != Phase::Ready {
            return Err(AppError::NotReady);
        }
        let data = SaveData { config: config.clone(), state: snapshot.state.clone(), view };
        Ok(self.repository.save(name, &data)?)
    }

    pub fn list_saves(&self) -> Result<Vec<SaveSummary>, AppError> {
        Ok(self.repository.list()?)
    }
}
