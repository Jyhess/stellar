//! Façade des cas d'usage, utilisée par la couche de présentation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use stellar_domain::{EngineDescriptor, SimulationConfig};

use crate::generator::generate_state;
use crate::ports::{RepositoryError, SaveData, SaveSummary, SimulationRepository, ViewState};
use crate::recording::{RecordingHeader, RecordingRepository, RecordingSummary};
use crate::registry::EngineRegistry;
use crate::replay::ReplayPlayer;
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

/// Résultat du chargement d'une sauvegarde.
pub struct LoadedSimulation {
    pub handle: SimulationHandle,
    pub view: Option<ViewState>,
    /// Reprise d'un enregistrement lié : chemin du fichier, ou erreur.
    pub recording: Option<Result<PathBuf, String>>,
}

/// Point d'entrée des cas d'usage : créer, charger, sauvegarder, lister.
#[derive(Clone)]
pub struct SimulatorService {
    registry: Arc<EngineRegistry>,
    repository: Arc<dyn SimulationRepository>,
    recordings: Arc<dyn RecordingRepository>,
}

impl SimulatorService {
    pub fn new(
        registry: EngineRegistry,
        repository: Arc<dyn SimulationRepository>,
        recordings: Arc<dyn RecordingRepository>,
    ) -> Self {
        Self {
            registry: Arc::new(registry),
            repository,
            recordings,
        }
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

    /// Charge une sauvegarde et la redémarre en pause. Si elle est liée à
    /// un enregistrement suspendu, celui-ci est rouvert et reprend dès que le
    /// calcul est relancé (les instantanés postérieurs sont supprimés).
    pub fn load_simulation(&self, path: &Path) -> Result<LoadedSimulation, AppError> {
        let data = self.repository.load(path)?;
        let factory = self
            .registry
            .get(&data.config.engine)
            .ok_or_else(|| AppError::UnknownEngine(data.config.engine.clone()))?;
        let SaveData {
            config,
            state,
            view,
            recording,
        } = data;
        let handle = SimulationHandle::start(config, factory, move || state, true);
        let recording = recording.map(|link| {
            let path = link.path.clone();
            self.recordings
                .resume(&link.path, link.last_frame_step)
                .map(|writer| {
                    handle.resume_recording(writer, link);
                    path
                })
                .map_err(|e| e.to_string())
        });
        Ok(LoadedSimulation {
            handle,
            view,
            recording,
        })
    }

    /// Suspend l'enregistrement en cours et sauvegarde la simulation au même
    /// instant, en liant les deux : recharger cette sauvegarde poursuivra
    /// l'enregistrement. `Ok(None)` si aucun enregistrement n'était actif.
    pub fn suspend_recording(
        &self,
        handle: &SimulationHandle,
        view: Option<ViewState>,
    ) -> Result<Option<PathBuf>, AppError> {
        let Some((link, state)) = handle.suspend_recording() else {
            return Ok(None);
        };
        let stem = link
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "enregistrement".into());
        let data = SaveData {
            config: handle.config().clone(),
            state,
            view,
            recording: Some(link),
        };
        Ok(Some(
            self.repository.save(&format!("{stem}_reprise"), &data)?,
        ))
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
        let data = SaveData {
            config: config.clone(),
            state: snapshot.state.clone(),
            view,
            recording: None,
        };
        Ok(self.repository.save(name, &data)?)
    }

    pub fn list_saves(&self) -> Result<Vec<SaveSummary>, AppError> {
        Ok(self.repository.list()?)
    }

    /// Démarre l'enregistrement d'une simulation : un instantané maintenant,
    /// puis tous les `interval` pas.
    pub fn start_recording(
        &self,
        name: &str,
        handle: &SimulationHandle,
        interval: u32,
        dust_budget: Option<u64>,
    ) -> Result<(), AppError> {
        let header = RecordingHeader {
            config: handle.config().clone(),
            interval: interval.max(1),
            start_step: handle.latest().state.step,
            created_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            dust_budget,
        };
        let writer = self.recordings.create(name, &header)?;
        handle.start_recording(writer, interval.max(1), dust_budget);
        Ok(())
    }

    pub fn list_recordings(&self) -> Result<Vec<RecordingSummary>, AppError> {
        Ok(self.recordings.list()?)
    }

    /// Ouvre un enregistrement pour relecture.
    pub fn open_replay(&self, path: &Path) -> Result<ReplayPlayer, AppError> {
        let reader = self.recordings.open(path)?;
        if reader.info().frames.is_empty() {
            return Err(AppError::Repository(RepositoryError::Format(
                "enregistrement vide".into(),
            )));
        }
        Ok(ReplayPlayer::new(reader))
    }
}
