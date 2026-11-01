//! Facade of the use cases, used by the presentation layer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use stellar_domain::{BodyId, EngineDescriptor, SimulationConfig};

use crate::generator::generate_state;
use crate::ports::{RepositoryError, SaveData, SaveSummary, SimulationRepository, ViewState};
use crate::recording::{
    is_dust, BodyFate, RecordingHeader, RecordingLink, RecordingRepository, RecordingSummary,
};
use crate::registry::EngineRegistry;
use crate::replay::ReplayPlayer;
use crate::runner::{Phase, SimulationHandle, Snapshot};

/// Name of a simulation from the name of one of its files: without the
/// "_reprise" suffix or the number added to make the name unique.
pub fn name_from_file(stem: &str) -> String {
    let base = stem.strip_suffix("_reprise").unwrap_or(stem);
    let base = match base.rsplit_once('_') {
        Some((head, n)) if !head.is_empty() && (1..=3).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => base,
    };
    base.to_owned()
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("moteur de calcul inconnu : « {0} »")]
    UnknownEngine(String),
    #[error("la simulation n'est pas encore prête")]
    NotReady,
    #[error("{0}")]
    EngineFailed(String),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

/// Result of loading a save.
pub struct LoadedSimulation {
    pub handle: SimulationHandle,
    pub view: Option<ViewState>,
    /// Resumption of a linked recording: path of the file, or error.
    pub recording: Option<Result<PathBuf, String>>,
}

/// Recording that can be continued from a save.
#[derive(Debug, Clone)]
pub struct Continuation {
    pub summary: RecordingSummary,
    /// Step of the last kept snapshot.
    pub resume_step: u64,
    /// Snapshots after the save, which will be deleted.
    pub dropped_frames: usize,
}

/// Entry point of the use cases: create, load, save, list.
#[derive(Clone)]
pub struct SimulatorService {
    registry: Arc<EngineRegistry>,
    repository: Arc<dyn SimulationRepository>,
    recordings: Arc<dyn RecordingRepository>,
    /// Note about the engines (graphics card detected or missing).
    engine_note: Option<String>,
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
            engine_note: None,
        }
    }

    pub fn set_engine_note(&mut self, note: String) {
        self.engine_note = Some(note);
    }

    pub fn engine_note(&self) -> Option<&str> {
        self.engine_note.as_deref()
    }

    /// Available computation engines.
    pub fn engines(&self) -> Vec<EngineDescriptor> {
        self.registry.descriptors()
    }

    /// Maximum number of bodies of engine `engine` (see
    /// [`stellar_domain::EngineFactory::max_bodies`]).
    pub fn max_bodies(&self, engine: &str) -> Option<usize> {
        self.registry.get(engine).and_then(|f| f.max_bodies())
    }

    pub fn default_engine(&self) -> Option<&'static str> {
        self.registry.default_id()
    }

    /// Creates and starts a new simulation. The cloud is generated on the
    /// simulation thread.
    pub fn new_simulation(&self, config: SimulationConfig) -> Result<SimulationHandle, AppError> {
        self.create_simulation(config, false)
    }

    /// Like [`Self::new_simulation`], but paused: enough to arm a recording
    /// before the first step, then restart the computation.
    pub fn new_simulation_paused(
        &self,
        config: SimulationConfig,
    ) -> Result<SimulationHandle, AppError> {
        self.create_simulation(config, true)
    }

    fn create_simulation(
        &self,
        config: SimulationConfig,
        paused: bool,
    ) -> Result<SimulationHandle, AppError> {
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
            paused,
        ))
    }

    /// Reads a save without starting it (a missing engine is replaced by the
    /// default engine).
    pub fn read_save(&self, path: &Path) -> Result<SaveData, AppError> {
        let mut data = self.repository.load(path)?;
        // Engine of the save missing (e.g. no graphics card): the default engine
        // takes over.
        if self.registry.get(&data.config.engine).is_none() {
            if let Some(id) = self.registry.default_id() {
                data.config.engine = id.to_owned();
            }
        }
        Ok(data)
    }

    /// Loads a save and restarts it paused. If it is linked to a recording
    /// (suspended, or running during an F5 save), that recording is reopened
    /// and resumes as soon as the computation restarts (later snapshots are
    /// deleted).
    pub fn load_simulation(&self, path: &Path) -> Result<LoadedSimulation, AppError> {
        let data = self.read_save(path)?;
        let loaded = self.start_loaded(data, None)?;
        if let Some(stem) = path.file_stem() {
            loaded.handle.set_name(&name_from_file(&stem.to_string_lossy()));
        }
        Ok(loaded)
    }

    /// Starts a save read by [`Self::read_save`]. `continue_with` designates a
    /// recording to continue; otherwise, the one the save is linked to.
    pub fn start_loaded(
        &self,
        data: SaveData,
        continue_with: Option<&Path>,
    ) -> Result<LoadedSimulation, AppError> {
        let factory = self
            .registry
            .get(&data.config.engine)
            .ok_or_else(|| AppError::UnknownEngine(data.config.engine.clone()))?;
        // Exact link (suspension) only if it designates the requested file.
        let exact = data
            .recording
            .clone()
            .filter(|l| continue_with.is_none_or(|p| p == l.path));
        let target = match &exact {
            Some(_) => None,
            None => continue_with
                .map(Path::to_path_buf)
                .or_else(|| data.continue_recording.clone()),
        };
        let continuation = target.map(|path| {
            self.continuation_link(&data, &path)
                .map(|(link, _)| link)
                .map_err(|e| (path, e))
        });
        let SaveData {
            config,
            state,
            view,
            ..
        } = data;
        let handle = SimulationHandle::start(config, factory, move || state, true);
        let link = match (exact, continuation) {
            (Some(l), _) => Some(Ok(l)),
            (None, Some(Ok(l))) => Some(Ok(l)),
            (None, Some(Err((_, e)))) => Some(Err(e)),
            (None, None) => None,
        };
        let recording = link.map(|link| {
            let link = link?;
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

    /// Recordings that can be continued from this save: same initial
    /// conditions, and at least one snapshot before or at the save's step.
    pub fn continuable_recordings(&self, data: &SaveData) -> Vec<Continuation> {
        let step = data.state.step;
        let Ok(list) = self.recordings.list() else {
            return Vec::new();
        };
        list.into_iter()
            .filter_map(|summary| {
                let reader = self.recordings.open(&summary.path).ok()?;
                let info = reader.info();
                if info.header.config.initial != data.config.initial {
                    return None;
                }
                let keep = info.frames.partition_point(|f| f.step <= step);
                (keep > 0).then(|| Continuation {
                    resume_step: info.frames[keep - 1].step,
                    dropped_frames: info.frames.len() - keep,
                    summary,
                })
            })
            .collect()
    }

    /// Builds the resume link of a recording from a save that is not exactly
    /// linked to it. Bodies that disappeared between the last kept snapshot and
    /// the save get their real fate if it appears in the next snapshot,
    /// "removed" otherwise. Also returns the number of snapshots that will be
    /// deleted.
    fn continuation_link(&self, data: &SaveData, path: &Path) -> Result<(RecordingLink, usize), String> {
        let step = data.state.step;
        let mut reader = self.recordings.open(path).map_err(|e| e.to_string())?;
        let info = reader.info().clone();
        if info.header.config.initial != data.config.initial {
            return Err("l'enregistrement provient d'une autre simulation (conditions initiales différentes)".into());
        }
        let keep = info.frames.partition_point(|f| f.step <= step);
        if keep == 0 {
            return Err("l'enregistrement commence après le pas de la sauvegarde".into());
        }
        let last = info.frames[keep - 1].step;
        let frame = reader.read_frame(keep - 1).map_err(|e| e.to_string())?;
        let recorded: HashSet<BodyId> = frame.bodies.ids.iter().copied().collect();
        let present: HashSet<BodyId> = data.state.bodies.ids.iter().copied().collect();
        // Bodies only ever disappear: every massive body of the save must appear
        // in the snapshot (dust, on the other hand, may have been subsampled).
        if let Some(id) = data
            .state
            .bodies
            .ids
            .iter()
            .zip(&data.state.bodies.masses)
            .find(|(id, &m)| !is_dust(m) && !recorded.contains(id))
            .map(|(id, _)| *id)
        {
            return Err(format!(
                "le corps n° {id} de la sauvegarde est absent de l'enregistrement au pas {last} : il provient d'une autre simulation"
            ));
        }
        let known: HashMap<BodyId, BodyFate> = if keep < info.frames.len() && last < step {
            reader
                .read_frame(keep)
                .map(|f| f.fates.into_iter().collect())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        drop(reader);
        let mut pending: Vec<(BodyId, BodyFate)> = frame
            .bodies
            .ids
            .iter()
            .filter(|id| !present.contains(id))
            .map(|&id| (id, known.get(&id).copied().unwrap_or(BodyFate::Removed)))
            .collect();
        pending.sort_unstable_by_key(|p| p.0);
        let interval = info.header.interval.max(1);
        let next_step = if last == step {
            step + interval as u64
        } else {
            step
        };
        Ok((
            RecordingLink {
                path: path.to_path_buf(),
                interval,
                dust_budget: info.header.dust_budget,
                last_frame_step: last,
                next_step,
                pending,
            },
            info.frames.len() - keep,
        ))
    }

    /// Suspends the running recording and saves the simulation at the same
    /// moment, linking both: reloading this save will continue the recording.
    /// `Ok(None)` if no recording was active.
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
            continue_recording: None,
        };
        Ok(Some(
            self.repository.save(&format!("{stem}_reprise"), &data)?,
        ))
    }

    /// Saves a snapshot. May be called from a background thread: the snapshot
    /// is immutable and shared. `recording`: running recording, which loading
    /// will continue.
    pub fn save(
        &self,
        name: &str,
        config: &SimulationConfig,
        snapshot: &Snapshot,
        view: Option<ViewState>,
        recording: Option<PathBuf>,
    ) -> Result<PathBuf, AppError> {
        if snapshot.phase != Phase::Ready {
            return Err(AppError::NotReady);
        }
        let data = SaveData {
            config: config.clone(),
            state: (*snapshot.state).clone(),
            view,
            recording: None,
            continue_recording: recording,
        };
        Ok(self.repository.save(name, &data)?)
    }

    pub fn list_saves(&self) -> Result<Vec<SaveSummary>, AppError> {
        Ok(self.repository.list()?)
    }

    /// Starts recording a simulation: a snapshot now, then every `interval`
    /// steps.
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
            start_step: handle.latest().step,
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

    /// Recording linked to a save (resumed or continued when it is loaded), if
    /// it is in the recordings folder.
    pub fn linked_recording(&self, save: &SaveSummary) -> Option<RecordingSummary> {
        let name = save.recording.as_deref()?;
        self.recordings
            .list()
            .ok()?
            .into_iter()
            .find(|r| r.path.file_name().is_some_and(|n| n.to_string_lossy() == name))
    }

    /// Saves linked to the recording `path`.
    pub fn linked_saves(&self, path: &Path) -> Vec<SaveSummary> {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return Vec::new();
        };
        self.repository
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.recording.as_deref() == Some(name.as_str()))
            .collect()
    }

    /// Deletes a save; `with_linked`: the linked recording too.
    pub fn delete_save(&self, save: &SaveSummary, with_linked: bool) -> Result<(), AppError> {
        let linked = with_linked.then(|| self.linked_recording(save)).flatten();
        self.repository.delete(&save.path)?;
        if let Some(r) = linked {
            self.recordings.delete(&r.path)?;
        }
        Ok(())
    }

    /// Deletes a recording; `with_linked`: the linked saves too.
    pub fn delete_recording(&self, rec: &RecordingSummary, with_linked: bool) -> Result<(), AppError> {
        let linked = if with_linked {
            self.linked_saves(&rec.path)
        } else {
            Vec::new()
        };
        self.recordings.delete(&rec.path)?;
        for s in linked {
            self.repository.delete(&s.path)?;
        }
        Ok(())
    }

    /// Renames a save. Its linked recording is renamed with it (without the
    /// "_reprise" suffix), as well as the other linked saves whose name starts
    /// with the recording's; every link is updated.
    pub fn rename_save(&self, save: &SaveSummary, name: &str) -> Result<(), AppError> {
        let linked = self.linked_recording(save);
        let path = self.repository.rename(&save.path, name)?;
        if let Some(rec) = linked {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let base = stem.strip_suffix("_reprise").unwrap_or(&stem).to_owned();
            self.rename_recording(&rec, &base)?;
        }
        Ok(())
    }

    /// Renames a recording; the linked saves follow (links updated, and name
    /// changed when it starts with the recording's).
    pub fn rename_recording(&self, rec: &RecordingSummary, name: &str) -> Result<(), AppError> {
        let saves = self.linked_saves(&rec.path);
        let new_path = self.recordings.rename(&rec.path, name)?;
        let new_stem = new_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for s in saves {
            self.repository.relink(&s.path, &rec.path, &new_path)?;
            if s.name != new_stem {
                if let Some(rest) = s.name.strip_prefix(rec.name.as_str()) {
                    self.repository.rename(&s.path, &format!("{new_stem}{rest}"))?;
                }
            }
        }
        Ok(())
    }

    /// Opens a recording for playback.
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

#[cfg(test)]
mod tests {
    use super::name_from_file;

    #[test]
    fn simulation_name_from_file_names() {
        assert_eq!(name_from_file("2026-10-28_1M_sph_orb"), "2026-10-28_1M_sph_orb");
        assert_eq!(name_from_file("2026-10-28_1M_sph_orb_2"), "2026-10-28_1M_sph_orb");
        assert_eq!(name_from_file("galaxy_reprise"), "galaxy");
        assert_eq!(name_from_file("galaxy_reprise_3"), "galaxy_reprise");
        assert_eq!(name_from_file("test_2026"), "test_2026");
    }
}

