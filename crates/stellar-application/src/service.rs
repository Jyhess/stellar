//! Façade des cas d'usage, utilisée par la couche de présentation.

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

/// Nom d'une simulation d'après le nom d'un de ses fichiers : sans le
/// suffixe « _reprise » ni le numéro ajouté pour rendre le nom unique.
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

/// Enregistrement que l'on peut poursuivre depuis une sauvegarde.
#[derive(Debug, Clone)]
pub struct Continuation {
    pub summary: RecordingSummary,
    /// Pas du dernier instantané conservé.
    pub resume_step: u64,
    /// Instantanés postérieurs à la sauvegarde, qui seront supprimés.
    pub dropped_frames: usize,
}

/// Point d'entrée des cas d'usage : créer, charger, sauvegarder, lister.
#[derive(Clone)]
pub struct SimulatorService {
    registry: Arc<EngineRegistry>,
    repository: Arc<dyn SimulationRepository>,
    recordings: Arc<dyn RecordingRepository>,
    /// Information sur les moteurs (carte graphique détectée ou absente).
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

    /// Moteurs de calcul disponibles.
    pub fn engines(&self) -> Vec<EngineDescriptor> {
        self.registry.descriptors()
    }

    /// Nombre maximal de corps du moteur `engine` (voir
    /// [`EngineFactory::max_bodies`]).
    pub fn max_bodies(&self, engine: &str) -> Option<usize> {
        self.registry.get(engine).and_then(|f| f.max_bodies())
    }

    pub fn default_engine(&self) -> Option<&'static str> {
        self.registry.default_id()
    }

    /// Crée et démarre une nouvelle simulation. La génération du nuage a lieu
    /// dans le thread de simulation.
    pub fn new_simulation(&self, config: SimulationConfig) -> Result<SimulationHandle, AppError> {
        self.create_simulation(config, false)
    }

    /// Comme [`Self::new_simulation`], mais en pause : de quoi armer un
    /// enregistrement avant le premier pas, puis relancer le calcul.
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

    /// Lit une sauvegarde sans la démarrer (le moteur absent est remplacé
    /// par le moteur par défaut).
    pub fn read_save(&self, path: &Path) -> Result<SaveData, AppError> {
        let mut data = self.repository.load(path)?;
        // Moteur de la sauvegarde absent (ex. pas de carte graphique) : le
        // moteur par défaut prend le relais.
        if self.registry.get(&data.config.engine).is_none() {
            if let Some(id) = self.registry.default_id() {
                data.config.engine = id.to_owned();
            }
        }
        Ok(data)
    }

    /// Charge une sauvegarde et la redémarre en pause. Si elle est liée à
    /// un enregistrement (suspendu, ou en cours lors d'une sauvegarde F5),
    /// celui-ci est rouvert et reprend dès que le calcul est relancé (les
    /// instantanés postérieurs sont supprimés).
    pub fn load_simulation(&self, path: &Path) -> Result<LoadedSimulation, AppError> {
        let data = self.read_save(path)?;
        let loaded = self.start_loaded(data, None)?;
        if let Some(stem) = path.file_stem() {
            loaded.handle.set_name(&name_from_file(&stem.to_string_lossy()));
        }
        Ok(loaded)
    }

    /// Démarre une sauvegarde lue par [`Self::read_save`]. `continue_with`
    /// désigne un enregistrement à poursuivre ; à défaut, celui auquel la
    /// sauvegarde est liée.
    pub fn start_loaded(
        &self,
        data: SaveData,
        continue_with: Option<&Path>,
    ) -> Result<LoadedSimulation, AppError> {
        let factory = self
            .registry
            .get(&data.config.engine)
            .ok_or_else(|| AppError::UnknownEngine(data.config.engine.clone()))?;
        // Lien exact (suspension) seulement s'il désigne le fichier demandé.
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

    /// Enregistrements qui peuvent être poursuivis depuis cette sauvegarde :
    /// mêmes conditions initiales, et au moins un instantané antérieur ou
    /// égal au pas de la sauvegarde.
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

    /// Construit le lien de reprise d'un enregistrement à partir d'une
    /// sauvegarde qui n'y est pas liée exactement. Les corps disparus entre
    /// le dernier instantané conservé et la sauvegarde reçoivent leur devenir
    /// réel s'il figure dans l'instantané suivant, « éliminé » sinon.
    /// Renvoie aussi le nombre d'instantanés qui seront supprimés.
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
        // Les corps ne font que disparaître : tout corps massif de la
        // sauvegarde doit figurer dans l'instantané (la poussière, elle, peut
        // avoir été sous-échantillonnée).
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
            continue_recording: None,
        };
        Ok(Some(
            self.repository.save(&format!("{stem}_reprise"), &data)?,
        ))
    }

    /// Sauvegarde un instantané. Peut être appelé depuis un thread
    /// d'arrière-plan : l'instantané est immuable et partagé. `recording` :
    /// enregistrement en cours, que le chargement poursuivra.
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

    /// Enregistrement lié à une sauvegarde (repris ou poursuivi à son
    /// chargement), s'il est dans le dossier des enregistrements.
    pub fn linked_recording(&self, save: &SaveSummary) -> Option<RecordingSummary> {
        let name = save.recording.as_deref()?;
        self.recordings
            .list()
            .ok()?
            .into_iter()
            .find(|r| r.path.file_name().is_some_and(|n| n.to_string_lossy() == name))
    }

    /// Sauvegardes liées à l'enregistrement `path`.
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

    /// Supprime une sauvegarde ; `with_linked` : aussi l'enregistrement lié.
    pub fn delete_save(&self, save: &SaveSummary, with_linked: bool) -> Result<(), AppError> {
        let linked = with_linked.then(|| self.linked_recording(save)).flatten();
        self.repository.delete(&save.path)?;
        if let Some(r) = linked {
            self.recordings.delete(&r.path)?;
        }
        Ok(())
    }

    /// Supprime un enregistrement ; `with_linked` : aussi les sauvegardes
    /// liées.
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

    /// Renomme une sauvegarde. Son enregistrement lié est renommé avec elle
    /// (sans le suffixe « _reprise »), ainsi que les autres sauvegardes liées
    /// dont le nom commence par celui de l'enregistrement ; tous les liens
    /// sont mis à jour.
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

    /// Renomme un enregistrement ; les sauvegardes liées suivent (liens mis à
    /// jour, et nom changé quand il commence par celui de l'enregistrement).
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

#[cfg(test)]
mod tests {
    use super::name_from_file;

    #[test]
    fn simulation_name_from_file_names() {
        assert_eq!(name_from_file("2026-10-28_1M_sph_orb"), "2026-10-28_1M_sph_orb");
        assert_eq!(name_from_file("2026-10-28_1M_sph_orb_2"), "2026-10-28_1M_sph_orb");
        assert_eq!(name_from_file("galaxie_reprise"), "galaxie");
        assert_eq!(name_from_file("galaxie_reprise_3"), "galaxie_reprise");
        assert_eq!(name_from_file("essai_2026"), "essai_2026");
    }
}

