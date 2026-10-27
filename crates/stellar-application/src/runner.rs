//! Pilotage d'une simulation dans un thread dédié, découplé de l'affichage.
//!
//! Le thread de simulation enchaîne les pas aussi vite que possible (ou à la
//! cadence maximale demandée) et publie régulièrement un [`Snapshot`]
//! immuable, que l'interface lit sans jamais bloquer le calcul.
//!
//! Quand le moteur offre un accès direct à ses données pour l'affichage
//! ([`PhysicsEngine::live_view`]), la copie complète de l'état, coûteuse,
//! n'est faite que toutes les une à deux secondes ; entre-temps, les
//! publications légères ne portent que les grandeurs globales et les
//! positions des corps suivis ([`SimulationHandle::set_watched`]).

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::recording::{
    BodyFate, Keyframe, Recorder, RecordingLink, RecordingStatus, RecordingWriter,
};
use crate::trace::{TraceReference, TraceSet};
use stellar_domain::{
    ranking, BodyId, Diagnostics, EngineFactory, PhysicsEngine, SimulationConfig, SimulationState,
    Vec3,
};

/// Nombre de corps listés dans le classement publié.
pub const TOP_COUNT: usize = 10;

/// Intervalle minimal entre deux publications (≈ 60 Hz).
const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);

/// Intervalle entre deux copies complètes de l'état quand l'affichage lit
/// directement les données du moteur : une seconde par tranche de trois
/// millions de corps, entre une et deux secondes.
fn full_state_interval(bodies: usize) -> Duration {
    Duration::from_secs_f64((bodies as f64 / 3e6).clamp(1.0, 2.0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Génération de l'état initial / construction du moteur en cours.
    Initialising,
    Ready,
    /// Le moteur n'a pas pu démarrer (message dans [`Snapshot::error`]).
    Failed,
}

/// Instantané publié par le thread de simulation.
///
/// `state` (et les grandeurs qui en dérivent : `top`, `total_mass`) peut
/// dater de quelques pas quand le moteur est affiché directement ; `step`,
/// `time`, `body_count`, `diagnostics`, `center_of_mass` et `watched` sont
/// toujours à jour.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub phase: Phase,
    /// Dernière copie complète de l'état (partagée entre publications).
    pub state: Arc<SimulationState>,
    /// Pas courant.
    pub step: u64,
    /// Temps simulé courant.
    pub time: f64,
    /// Nombre de corps courant.
    pub body_count: usize,
    pub diagnostics: Diagnostics,
    /// Centre de masse courant, si le moteur le fournit à moindre coût.
    pub center_of_mass: Option<Vec3>,
    /// Positions courantes des corps suivis (absents : éliminés ou fusionnés).
    pub watched: Vec<(BodyId, Vec3)>,
    /// Classement des corps les plus massifs : (id, masse).
    pub top: Vec<(BodyId, f64)>,
    /// Masse totale des corps présents dans la simulation.
    pub total_mass: f64,
    /// Cadence de calcul (moyenne glissante).
    pub steps_per_second: f64,
    /// Durée du dernier pas, en millisecondes.
    pub last_step_ms: f64,
    pub running: bool,
    /// Numéro de publication, strictement croissant.
    pub sequence: u64,
    /// Message d'erreur si le moteur n'a pas pu démarrer.
    pub error: Option<String>,
}

impl Snapshot {
    fn initialising(running: bool) -> Self {
        Self {
            phase: Phase::Initialising,
            state: Arc::new(SimulationState::default()),
            step: 0,
            time: 0.0,
            body_count: 0,
            diagnostics: Diagnostics::default(),
            center_of_mass: None,
            watched: Vec::new(),
            top: Vec::new(),
            total_mass: 0.0,
            steps_per_second: 0.0,
            last_step_ms: 0.0,
            running,
            sequence: 0,
            error: None,
        }
    }

    /// Masse sortie de la simulation (corps éjectés puis éliminés).
    pub fn ejected_mass(&self) -> Option<f64> {
        let m0 = self.state.initial_mass?;
        (self.phase == Phase::Ready).then(|| (m0 - self.total_mass).max(0.0))
    }

    /// Part de la masse présente portée par un corps de masse `m`.
    pub fn mass_fraction(&self, m: f64) -> f64 {
        if self.total_mass > 0.0 {
            m / self.total_mass
        } else {
            0.0
        }
    }

    /// Position courante d'un corps suivi ([`SimulationHandle::set_watched`]).
    pub fn watched_position(&self, id: BodyId) -> Option<Vec3> {
        self.watched.iter().find(|(w, _)| *w == id).map(|(_, p)| *p)
    }

    /// Position courante d'un corps : suivie si possible, sinon tirée de la
    /// dernière copie complète de l'état (recherche linéaire).
    pub fn position_of(&self, id: BodyId) -> Option<Vec3> {
        self.watched_position(id).or_else(|| {
            let b = &self.state.bodies;
            b.index_of(id).map(|i| b.positions[i])
        })
    }

    /// Variation relative de l'énergie totale depuis l'origine.
    pub fn energy_drift(&self) -> Option<f64> {
        let e0 = self.state.initial_energy?;
        (e0 != 0.0).then(|| (self.diagnostics.total_energy() - e0) / e0.abs())
    }
}

enum Command {
    Pause,
    Resume,
    StepOnce,
    SetMaxRate(Option<f64>),
    StartTraces(Vec<BodyId>),
    SetWatched(Vec<BodyId>),
    StopTraces,
    SetTraceReference(Option<TraceReference>),
    StartRecording {
        writer: Box<dyn RecordingWriter>,
        interval: u32,
        dust_budget: Option<u64>,
    },
    StopRecording,
    /// Clôt l'enregistrement et renvoie de quoi le poursuivre, avec l'état
    /// exact de la simulation au même instant.
    SuspendRecording(Sender<Option<(RecordingLink, SimulationState)>>),
    ResumeRecording {
        writer: Box<dyn RecordingWriter>,
        link: RecordingLink,
    },
    Shutdown,
}

/// Devenir d'un corps disparu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    MergedInto(BodyId),
    Removed,
}

struct Shared {
    latest: Mutex<Arc<Snapshot>>,
    sequence: AtomicU64,
    fates: Mutex<HashMap<BodyId, Fate>>,
    traces: Mutex<Option<TraceSet>>,
    tracing: AtomicBool,
    recording: Arc<RecordingStatus>,
    /// Accès direct du moteur pour l'affichage, une fois le moteur créé.
    live_view: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
}

/// Poignée de pilotage d'une simulation en cours. L'arrêter (ou la détruire)
/// termine proprement le thread de calcul.
pub struct SimulationHandle {
    config: SimulationConfig,
    tx: Sender<Command>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SimulationHandle {
    /// Démarre une simulation. `source` produit l'état initial ; il est
    /// appelé dans le thread de simulation pour ne pas figer l'interface.
    pub fn start<F>(
        config: SimulationConfig,
        factory: Arc<dyn EngineFactory>,
        source: F,
        start_paused: bool,
    ) -> Self
    where
        F: FnOnce() -> SimulationState + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            latest: Mutex::new(Arc::new(Snapshot::initialising(!start_paused))),
            sequence: AtomicU64::new(0),
            fates: Mutex::new(HashMap::new()),
            traces: Mutex::new(None),
            tracing: AtomicBool::new(false),
            recording: Arc::new(RecordingStatus::default()),
            live_view: Mutex::new(None),
        });
        let worker_shared = Arc::clone(&shared);
        let params = config.physics;
        let initial_count = config.initial.count;
        let thread = std::thread::Builder::new()
            .name("stellar-simulation".into())
            .spawn(move || {
                let state = source();
                let saved_energy = state.initial_energy;
                // Sauvegardes antérieures : masse initiale déduite des conditions initiales.
                let initial_mass = state.initial_mass.or(Some(initial_count as f64));
                let step = state.step;
                let engine = match factory.create(state, params) {
                    Ok(e) => e,
                    Err(message) => {
                        let mut s = Snapshot::initialising(false);
                        s.phase = Phase::Failed;
                        s.error = Some(message);
                        s.sequence = 1;
                        *worker_shared.latest.lock().expect("verrou empoisonné") = Arc::new(s);
                        worker_shared.sequence.store(1, Ordering::Release);
                        // Attente de l'arrêt demandé par l'interface.
                        while let Ok(cmd) = rx.recv() {
                            if matches!(cmd, Command::Shutdown) {
                                break;
                            }
                        }
                        return;
                    }
                };
                let initial_energy =
                    saved_energy.or_else(|| Some(engine.diagnostics().total_energy()));
                let live_view = engine.live_view();
                let direct_view = live_view.is_some();
                *worker_shared.live_view.lock().expect("verrou empoisonné") = live_view;
                Worker {
                    engine,
                    rx,
                    shared: worker_shared,
                    running: !start_paused,
                    max_rate: None,
                    initial_energy,
                    initial_mass,
                    steps_per_second: 0.0,
                    last_step_ms: 0.0,
                    last_publish: None,
                    last_full: None,
                    direct_view,
                    watched: Vec::new(),
                    full: Arc::new(SimulationState::default()),
                    top: Vec::new(),
                    total_mass: 0.0,
                    recent: VecDeque::new(),
                    spare: None,
                    step,
                    dt: params.dt,
                    recording: None,
                }
                .run();
            })
            .expect("impossible de lancer le thread de simulation");
        Self {
            config,
            tx,
            shared,
            thread: Some(thread),
        }
    }

    pub fn config(&self) -> &SimulationConfig {
        &self.config
    }

    /// Dernier instantané publié (lecture non bloquante pour le calcul).
    pub fn latest(&self) -> Arc<Snapshot> {
        Arc::clone(&self.shared.latest.lock().expect("verrou empoisonné"))
    }

    /// Accès direct du moteur pour l'affichage (`None` tant que le moteur
    /// n'est pas créé, ou s'il n'en offre pas). Voir [`PhysicsEngine::live_view`].
    pub fn live_view(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.shared
            .live_view
            .lock()
            .expect("verrou empoisonné")
            .clone()
    }

    /// Corps dont la position est publiée à chaque instantané
    /// ([`Snapshot::watched`]), typiquement le corps sélectionné.
    pub fn set_watched(&self, ids: Vec<BodyId>) {
        let _ = self.tx.send(Command::SetWatched(ids));
    }

    /// Numéro du dernier instantané publié.
    pub fn sequence(&self) -> u64 {
        self.shared.sequence.load(Ordering::Acquire)
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }

    pub fn resume(&self) {
        let _ = self.tx.send(Command::Resume);
    }

    /// Effectue un unique pas (utile en pause).
    pub fn step_once(&self) {
        let _ = self.tx.send(Command::StepOnce);
    }

    /// Limite la cadence de calcul (pas par seconde) ; `None` = illimitée.
    pub fn set_max_rate(&self, rate: Option<f64>) {
        let _ = self.tx.send(Command::SetMaxRate(rate));
    }

    /// Démarre l'enregistrement des traces des corps donnés (un point par
    /// pas, à partir de maintenant). Remplace les traces existantes.
    pub fn start_traces(&self, ids: Vec<BodyId>) {
        let _ = self.tx.send(Command::StartTraces(ids));
    }

    /// Démarre l'enregistrement : un instantané maintenant, puis tous les
    /// `interval` pas. Remplace un éventuel enregistrement en cours.
    pub fn start_recording(
        &self,
        writer: Box<dyn RecordingWriter>,
        interval: u32,
        dust_budget: Option<u64>,
    ) {
        let _ = self.tx.send(Command::StartRecording {
            writer,
            interval,
            dust_budget,
        });
    }

    /// Suspend l'enregistrement en cours (fichier clos proprement) et renvoie
    /// le lien de reprise ainsi que l'état de la simulation au même pas.
    /// `None` si aucun enregistrement n'est actif. Bloquant.
    pub fn suspend_recording(&self) -> Option<(RecordingLink, SimulationState)> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Command::SuspendRecording(tx)).ok()?;
        rx.recv().ok().flatten()
    }

    /// Poursuit un enregistrement suspendu.
    pub fn resume_recording(&self, writer: Box<dyn RecordingWriter>, link: RecordingLink) {
        let _ = self.tx.send(Command::ResumeRecording { writer, link });
    }

    /// Vrai si un enregistrement est en cours.
    pub fn is_recording(&self) -> bool {
        self.shared.recording.active.load(Ordering::Acquire)
    }

    /// Arrête l'enregistrement (le fichier est clos par le thread de calcul).
    pub fn stop_recording(&self) {
        let _ = self.tx.send(Command::StopRecording);
    }

    /// État de l'enregistrement (instantanés écrits, octets, erreurs…).
    pub fn recording_status(&self) -> &RecordingStatus {
        &self.shared.recording
    }

    /// Corps de référence pour l'affichage relatif des traces.
    pub fn set_trace_reference(&self, id: Option<TraceReference>) {
        let _ = self.tx.send(Command::SetTraceReference(id));
    }

    /// Arrête et efface les traces.
    pub fn stop_traces(&self) {
        let _ = self.tx.send(Command::StopTraces);
    }

    /// Accès en lecture aux traces (verrou bref : copier ce qui est utile).
    pub fn traces(&self) -> MutexGuard<'_, Option<TraceSet>> {
        self.shared.traces.lock().expect("verrou empoisonné")
    }

    /// Identifiant actuel d'un corps, en suivant la chaîne des fusions.
    /// `None` si le corps a été éliminé (éjection).
    pub fn resolve(&self, id: BodyId) -> Option<BodyId> {
        let fates = self.shared.fates.lock().expect("verrou empoisonné");
        let mut cur = id;
        for _ in 0..1_000 {
            match fates.get(&cur) {
                None => return Some(cur),
                Some(Fate::MergedInto(next)) => cur = *next,
                Some(Fate::Removed) => return None,
            }
        }
        Some(cur)
    }

    /// Arrête le thread de simulation et attend sa fin.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SimulationHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker {
    engine: Box<dyn PhysicsEngine>,
    rx: Receiver<Command>,
    shared: Arc<Shared>,
    running: bool,
    max_rate: Option<f64>,
    initial_energy: Option<f64>,
    initial_mass: Option<f64>,
    steps_per_second: f64,
    last_step_ms: f64,
    last_publish: Option<Instant>,
    /// Dernière copie complète de l'état.
    last_full: Option<Instant>,
    /// L'affichage lit directement les données du moteur : copies
    /// complètes espacées, publications légères entre-temps.
    direct_view: bool,
    /// Corps dont la position accompagne chaque publication.
    watched: Vec<BodyId>,
    /// Dernière copie complète publiée, et grandeurs qui en dérivent.
    full: Arc<SimulationState>,
    top: Vec<(BodyId, f64)>,
    total_mass: f64,
    /// Copies récentes, dont les tableaux sont recyclés quand l'interface
    /// les a lâchées (évite de réallouer des centaines de mégaoctets).
    recent: VecDeque<Arc<SimulationState>>,
    spare: Option<SimulationState>,
    /// Numéro du pas courant (pour dater les points des traces).
    step: u64,
    /// Pas de temps (fixe).
    dt: f64,
    recording: Option<ActiveRecording>,
}

/// Enregistrement en cours : un instantané tous les `interval` pas.
struct ActiveRecording {
    recorder: Recorder,
    interval: u64,
    next_step: u64,
    /// Devenirs accumulés depuis le dernier instantané.
    pending: Vec<(BodyId, BodyFate)>,
    dust_budget: Option<u64>,
    last_frame_step: u64,
}

impl Worker {
    fn run(mut self) {
        self.run_loop();
        // Fin du thread : un enregistrement en cours est clos proprement.
        self.stop_recording();
    }

    fn start_recording(
        &mut self,
        writer: Box<dyn RecordingWriter>,
        interval: u32,
        dust_budget: Option<u64>,
    ) {
        self.stop_recording();
        let status = &self.shared.recording;
        status.frames.store(0, Ordering::Relaxed);
        status.bytes.store(0, Ordering::Relaxed);
        status
            .interval
            .store(interval.max(1) as u64, Ordering::Relaxed);
        *status.error.lock().expect("verrou empoisonné") = None;
        let recorder = Recorder::start(writer, Arc::clone(status), dust_budget);
        let mut rec = ActiveRecording {
            recorder,
            interval: interval.max(1) as u64,
            next_step: self.step,
            pending: Vec::new(),
            dust_budget,
            last_frame_step: self.step,
        };
        self.record_keyframe(&mut rec);
        self.recording = Some(rec);
    }

    fn resume_recording(&mut self, writer: Box<dyn RecordingWriter>, link: RecordingLink) {
        self.stop_recording();
        let status = &self.shared.recording;
        status.frames.store(0, Ordering::Relaxed);
        status.bytes.store(0, Ordering::Relaxed);
        status
            .interval
            .store(link.interval.max(1) as u64, Ordering::Relaxed);
        *status.error.lock().expect("verrou empoisonné") = None;
        let recorder = Recorder::start(writer, Arc::clone(status), link.dust_budget);
        let mut rec = ActiveRecording {
            recorder,
            interval: link.interval.max(1) as u64,
            next_step: link.next_step,
            pending: link.pending,
            dust_budget: link.dust_budget,
            last_frame_step: link.last_frame_step,
        };
        // Poursuite d'un enregistrement depuis une sauvegarde non liée : un
        // instantané est écrit aussitôt, au pas de la sauvegarde.
        if rec.next_step <= self.step {
            self.record_keyframe(&mut rec);
        }
        self.recording = Some(rec);
    }

    fn suspend_recording(&mut self) -> Option<(RecordingLink, SimulationState)> {
        let rec = self.recording.take()?;
        let interval = rec.interval as u32;
        let (next_step, pending, dust_budget, last) = (
            rec.next_step,
            rec.pending,
            rec.dust_budget,
            rec.last_frame_step,
        );
        let path = rec.recorder.finish().ok()?;
        let mut state = SimulationState::default();
        self.engine.write_state(&mut state);
        state.initial_energy = self.initial_energy;
        state.initial_mass = self.initial_mass;
        Some((
            RecordingLink {
                path,
                interval,
                dust_budget,
                last_frame_step: last,
                next_step,
                pending,
            },
            state,
        ))
    }

    fn stop_recording(&mut self) {
        if let Some(rec) = self.recording.take() {
            let _ = rec.recorder.finish();
        }
    }

    fn record_keyframe(&self, rec: &mut ActiveRecording) {
        let mut s = SimulationState::default();
        self.engine.write_state(&mut s);
        rec.recorder.push(Keyframe {
            step: self.step,
            time: s.time,
            bodies: s.bodies,
            fates: std::mem::take(&mut rec.pending),
            dust_weight: 1.0,
        });
        rec.last_frame_step = self.step;
        rec.next_step = self.step + rec.interval;
    }

    fn run_loop(&mut self) {
        self.publish(true);
        loop {
            // 1. Traitement des commandes (bloquant en pause).
            loop {
                let cmd = if self.running {
                    match self.rx.try_recv() {
                        Ok(c) => c,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                } else {
                    match self.rx.recv() {
                        Ok(c) => c,
                        Err(_) => return,
                    }
                };
                match cmd {
                    Command::Pause => {
                        self.running = false;
                        self.publish(true);
                    }
                    Command::Resume => self.running = true,
                    Command::StepOnce => {
                        self.step();
                        self.publish(true);
                    }
                    Command::SetWatched(ids) => {
                        self.watched = ids;
                        if !self.running {
                            self.publish(false);
                        }
                    }
                    Command::SetMaxRate(r) => self.max_rate = r.filter(|r| *r > 0.0),
                    Command::StartTraces(ids) => {
                        let mut set = TraceSet::new(ids);
                        let pos = self.engine.positions_of(&set.current_ids());
                        set.push(self.step, &pos);
                        if let Some(c) = self.engine.center_of_mass() {
                            set.push_barycenter(self.step, c);
                        }
                        *self.shared.traces.lock().expect("verrou empoisonné") = Some(set);
                        self.shared.tracing.store(true, Ordering::Release);
                    }
                    Command::SetTraceReference(id) => {
                        let mut guard = self.shared.traces.lock().expect("verrou empoisonné");
                        if let Some(set) = guard.as_mut() {
                            set.set_reference_kind(id);
                            let ids = set.current_ids();
                            let pos = self.engine.positions_of(&ids);
                            set.push(self.step, &pos);
                        }
                    }
                    Command::StartRecording {
                        writer,
                        interval,
                        dust_budget,
                    } => self.start_recording(writer, interval, dust_budget),
                    Command::StopRecording => self.stop_recording(),
                    Command::SuspendRecording(reply) => {
                        let _ = reply.send(self.suspend_recording());
                    }
                    Command::ResumeRecording { writer, link } => {
                        self.resume_recording(writer, link)
                    }
                    Command::StopTraces => {
                        self.shared.tracing.store(false, Ordering::Release);
                        *self.shared.traces.lock().expect("verrou empoisonné") = None;
                    }
                    Command::Shutdown => return,
                }
            }

            // 2. Un pas de calcul.
            let started = Instant::now();
            self.step();
            let n = self.engine.body_count();
            if self.direct_view {
                // Affichage direct : copie complète toutes les une à deux
                // secondes, publications légères entre-temps.
                let full = self
                    .last_full
                    .map_or(true, |t| t.elapsed() >= full_state_interval(n));
                if full
                    || self
                        .last_publish
                        .map_or(true, |t| t.elapsed() >= PUBLISH_INTERVAL)
                {
                    self.publish(full);
                }
            } else {
                // Au-delà d'un million de corps, la copie de l'état devient
                // coûteuse : publications plus espacées.
                let scale = (n as f64 / 1e6).max(1.0);
                if self
                    .last_publish
                    .map_or(true, |t| t.elapsed() >= PUBLISH_INTERVAL.mul_f64(scale))
                {
                    self.publish(true);
                }
            }

            // 3. Limitation éventuelle de cadence.
            if let Some(rate) = self.max_rate {
                let target = Duration::from_secs_f64(1.0 / rate);
                let spent = started.elapsed();
                if spent < target {
                    std::thread::sleep(target - spent);
                }
            }
        }
    }

    fn step(&mut self) {
        let t0 = Instant::now();
        let report = self.engine.step();
        self.step += 1;
        let dt = t0.elapsed().as_secs_f64();
        self.last_step_ms = dt * 1e3;
        let inst = if dt > 0.0 { 1.0 / dt } else { 0.0 };
        self.steps_per_second = if self.steps_per_second == 0.0 {
            inst
        } else {
            0.9 * self.steps_per_second + 0.1 * inst
        };
        if self.shared.tracing.load(Ordering::Acquire) {
            let mut guard = self.shared.traces.lock().expect("verrou empoisonné");
            if let Some(set) = guard.as_mut() {
                set.apply_fates(&report.merges, &report.removed);
                let ids = set.current_ids();
                if !ids.is_empty() {
                    let pos = self.engine.positions_of(&ids);
                    set.push(self.step, &pos);
                }
                if let Some(c) = self.engine.center_of_mass() {
                    set.push_barycenter(self.step, c);
                }
            }
        }
        if let Some(mut rec) = self.recording.take() {
            rec.pending.extend(
                report
                    .merges
                    .iter()
                    .map(|&(a, s)| (a, BodyFate::MergedInto(s))),
            );
            rec.pending
                .extend(report.removed.iter().map(|&id| (id, BodyFate::Removed)));
            if self.step >= rec.next_step {
                self.record_keyframe(&mut rec);
            }
            self.recording = Some(rec);
        }
        if !report.merges.is_empty() || !report.removed.is_empty() {
            let mut fates = self.shared.fates.lock().expect("verrou empoisonné");
            for (absorbed, survivor) in report.merges {
                fates.insert(absorbed, Fate::MergedInto(survivor));
            }
            for id in report.removed {
                fates.insert(id, Fate::Removed);
            }
        }
    }

    /// Copie complète de l'état, dans les tableaux d'une copie que plus
    /// personne n'utilise quand c'est possible.
    fn refresh_full_state(&mut self) {
        // La copie publiée en dernier est encore affichée ; les précédentes
        // ont en principe été lâchées par l'interface.
        while self.recent.len() > 1 {
            if let Some(old) = self.recent.pop_front() {
                if let Ok(s) = Arc::try_unwrap(old) {
                    self.spare = Some(s);
                }
            }
        }
        let mut state = self.spare.take().unwrap_or_default();
        self.engine.write_state(&mut state);
        state.initial_energy = self.initial_energy;
        state.initial_mass = self.initial_mass;
        self.total_mass = state.bodies.total_mass();
        self.top = ranking::top_k(&state.bodies.ids, &state.bodies.masses, TOP_COUNT);
        self.full = Arc::new(state);
        self.recent.push_back(Arc::clone(&self.full));
        self.last_full = Some(Instant::now());
    }

    /// Publie un instantané ; `full` : avec une nouvelle copie complète de
    /// l'état (sinon, la précédente est reprise).
    fn publish(&mut self, full: bool) {
        if full {
            self.refresh_full_state();
        }
        let watched = if self.watched.is_empty() {
            Vec::new()
        } else {
            let pos = self.engine.positions_of(&self.watched);
            self.watched
                .iter()
                .zip(pos)
                .filter_map(|(&id, p)| p.map(|p| (id, p)))
                .collect()
        };
        // Sans affichage direct, l'interface le déduit de l'état complet.
        let center_of_mass = if self.direct_view {
            self.engine.center_of_mass()
        } else {
            None
        };
        let sequence = self.shared.sequence.load(Ordering::Relaxed) + 1;
        let snapshot = Arc::new(Snapshot {
            phase: Phase::Ready,
            state: Arc::clone(&self.full),
            step: self.step,
            // Pas de temps fixe : temps courant déduit de la dernière copie.
            time: self.full.time + (self.step as f64 - self.full.step as f64) * self.dt,
            body_count: self.engine.body_count(),
            diagnostics: self.engine.diagnostics(),
            center_of_mass,
            watched,
            top: self.top.clone(),
            total_mass: self.total_mass,
            steps_per_second: self.steps_per_second,
            last_step_ms: self.last_step_ms,
            running: self.running,
            sequence,
            error: None,
        });
        *self.shared.latest.lock().expect("verrou empoisonné") = snapshot;
        self.shared.sequence.store(sequence, Ordering::Release);
        self.last_publish = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::RepositoryError;
    use std::path::PathBuf;
    use stellar_domain::{BodySet, EngineDescriptor, PhysicsParams, StepReport, Vec3};

    /// Moteur factice : translation uniforme, le corps 2 absorbé au pas 3.
    struct Drift {
        state: SimulationState,
    }

    impl PhysicsEngine for Drift {
        fn step(&mut self) -> StepReport {
            self.state.step += 1;
            self.state.time += 1.0;
            for p in &mut self.state.bodies.positions {
                p.x += 1.0;
            }
            let mut report = StepReport::default();
            if self.state.step == 3 {
                let b = &mut self.state.bodies;
                b.ids.pop();
                b.positions.pop();
                b.velocities.pop();
                b.masses.pop();
                b.masses[0] += 1.0;
                report.merges.push((2, 1));
            }
            report
        }
        fn state(&self) -> SimulationState {
            self.state.clone()
        }
        fn body_count(&self) -> usize {
            self.state.bodies.len()
        }
        fn diagnostics(&self) -> Diagnostics {
            Diagnostics::default()
        }
    }

    struct DriftFactory;
    impl EngineFactory for DriftFactory {
        fn descriptor(&self) -> EngineDescriptor {
            EngineDescriptor {
                id: "drift",
                label: "drift",
                description: "",
            }
        }
        fn create(
            &self,
            state: SimulationState,
            _: PhysicsParams,
        ) -> Result<Box<dyn PhysicsEngine>, String> {
            Ok(Box::new(Drift { state }))
        }
    }

    /// Même moteur, mais qui se déclare affiché directement.
    struct DirectDrift(Drift);

    impl PhysicsEngine for DirectDrift {
        fn step(&mut self) -> StepReport {
            self.0.step()
        }
        fn state(&self) -> SimulationState {
            self.0.state()
        }
        fn body_count(&self) -> usize {
            self.0.body_count()
        }
        fn diagnostics(&self) -> Diagnostics {
            self.0.diagnostics()
        }
        fn live_view(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
            Some(Arc::new(()))
        }
    }

    struct DirectDriftFactory;
    impl EngineFactory for DirectDriftFactory {
        fn descriptor(&self) -> EngineDescriptor {
            EngineDescriptor {
                id: "direct-drift",
                label: "direct-drift",
                description: "",
            }
        }
        fn create(
            &self,
            state: SimulationState,
            _: PhysicsParams,
        ) -> Result<Box<dyn PhysicsEngine>, String> {
            Ok(Box::new(DirectDrift(Drift { state })))
        }
    }

    /// Affichage direct : l'état complet n'est recopié que toutes les une à
    /// deux secondes, mais le pas, le centre de masse et les corps suivis
    /// sont publiés à jour.
    #[test]
    fn direct_view_publishes_light_snapshots_between_full_copies() {
        let mut bodies = BodySet::default();
        bodies.push(1, Vec3::ZERO, Vec3::X, 1.0);
        bodies.push(2, Vec3::Y, Vec3::X, 1.0);
        let state = SimulationState {
            bodies,
            ..Default::default()
        };
        let config = SimulationConfig {
            engine: "direct-drift".into(),
            ..Default::default()
        };
        let handle =
            SimulationHandle::start(config, Arc::new(DirectDriftFactory), move || state, true);
        handle.set_watched(vec![1]);
        handle.resume();
        wait_for(&handle, 50);
        let snap = handle.latest();
        assert!(handle.live_view().is_some());
        // Copie complète initiale (pas 0), pas encore renouvelée.
        assert_eq!(snap.state.step, 0);
        assert!(snap.step >= 50);
        assert_eq!(snap.body_count, 1);
        // Le corps 1 avance d'une unité par pas ; il a absorbé le corps 2.
        let p = snap.watched_position(1).expect("corps suivi");
        assert_eq!(p.x, snap.step as f64);
        assert!(snap.center_of_mass.is_some());
        handle.pause();
        // La pause publie une copie complète à jour.
        for _ in 0..500 {
            let s = handle.latest();
            if !s.running && s.state.step == s.step {
                handle.shutdown();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("pas de copie complète à la pause");
    }

    #[derive(Clone, Default)]
    struct MemWriter(Arc<Mutex<Vec<Keyframe>>>);
    impl RecordingWriter for MemWriter {
        fn path(&self) -> PathBuf {
            PathBuf::from("mem.stellarec")
        }
        fn write_frame(&mut self, frame: &Keyframe) -> Result<u64, RepositoryError> {
            self.0.lock().unwrap().push(frame.clone());
            Ok(1)
        }
        fn finish(self: Box<Self>) -> Result<PathBuf, RepositoryError> {
            Ok(PathBuf::from("mem.stellarec"))
        }
    }

    fn wait_for(handle: &SimulationHandle, step: u64) {
        for _ in 0..500 {
            if handle.latest().step >= step {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("pas {step} jamais atteint");
    }

    #[test]
    fn suspend_and_resume_keep_recording_continuous() {
        let mut bodies = BodySet::default();
        bodies.push(1, Vec3::ZERO, Vec3::X, 1.0);
        bodies.push(2, Vec3::Y, Vec3::X, 1.0);
        let state = SimulationState {
            bodies,
            initial_mass: Some(2.0),
            ..Default::default()
        };
        let config = SimulationConfig {
            engine: "drift".into(),
            ..Default::default()
        };
        let handle =
            SimulationHandle::start(config.clone(), Arc::new(DriftFactory), move || state, true);

        let mem = MemWriter::default();
        handle.start_recording(Box::new(mem.clone()), 2, None);
        for k in 1..=5 {
            handle.step_once();
            wait_for(&handle, k);
        }
        let (link, saved) = handle.suspend_recording().expect("enregistrement actif");
        assert!(!handle.is_recording());
        let steps: Vec<u64> = mem.0.lock().unwrap().iter().map(|f| f.step).collect();
        assert_eq!(steps, vec![0, 2, 4]);
        assert_eq!(saved.step, 5);
        assert_eq!(link.last_frame_step, 4);
        assert_eq!(link.next_step, 6);
        assert_eq!(link.interval, 2);
        assert_eq!(saved.initial_mass, Some(2.0));
        // La fusion du pas 3 figure dans l'instantané du pas 4.
        assert_eq!(
            mem.0.lock().unwrap()[2].fates,
            vec![(2, BodyFate::MergedInto(1))]
        );
        handle.shutdown();

        // Reprise depuis la sauvegarde : l'instantané suivant est au pas 6.
        let handle = SimulationHandle::start(config, Arc::new(DriftFactory), move || saved, true);
        let mem2 = MemWriter::default();
        handle.resume_recording(Box::new(mem2.clone()), link);
        for k in 6..=8 {
            handle.step_once();
            wait_for(&handle, k);
        }
        handle.stop_recording();
        let snap = handle.latest();
        assert_eq!(snap.ejected_mass(), Some(0.0));
        handle.shutdown();
        let steps: Vec<u64> = mem2.0.lock().unwrap().iter().map(|f| f.step).collect();
        assert_eq!(steps, vec![6, 8]);
    }
}
