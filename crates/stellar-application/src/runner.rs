//! Pilotage d'une simulation dans un thread dédié, découplé de l'affichage.
//!
//! Le thread de simulation enchaîne les pas aussi vite que possible (ou à la
//! cadence maximale demandée) et publie régulièrement un [`Snapshot`]
//! immuable, que l'interface lit sans jamais bloquer le calcul.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::recording::{BodyFate, Keyframe, Recorder, RecordingStatus, RecordingWriter};
use crate::trace::TraceSet;
use stellar_domain::{
    ranking, BodyId, Diagnostics, EngineFactory, PhysicsEngine, SimulationConfig, SimulationState,
};

/// Nombre de corps listés dans le classement publié.
pub const TOP_COUNT: usize = 10;

/// Intervalle minimal entre deux publications (≈ 60 Hz).
const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Génération de l'état initial / construction du moteur en cours.
    Initialising,
    Ready,
}

/// Instantané publié par le thread de simulation.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub phase: Phase,
    pub state: SimulationState,
    pub diagnostics: Diagnostics,
    /// Classement des corps les plus massifs : (id, masse).
    pub top: Vec<(BodyId, f64)>,
    /// Cadence de calcul (moyenne glissante).
    pub steps_per_second: f64,
    /// Durée du dernier pas, en millisecondes.
    pub last_step_ms: f64,
    pub running: bool,
    /// Numéro de publication, strictement croissant.
    pub sequence: u64,
}

impl Snapshot {
    fn initialising(running: bool) -> Self {
        Self {
            phase: Phase::Initialising,
            state: SimulationState::default(),
            diagnostics: Diagnostics::default(),
            top: Vec::new(),
            steps_per_second: 0.0,
            last_step_ms: 0.0,
            running,
            sequence: 0,
        }
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
    StopTraces,
    SetTraceReference(Option<BodyId>),
    StartRecording {
        writer: Box<dyn RecordingWriter>,
        interval: u32,
        dust_budget: Option<u64>,
    },
    StopRecording,
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
        });
        let worker_shared = Arc::clone(&shared);
        let params = config.physics;
        let thread = std::thread::Builder::new()
            .name("stellar-simulation".into())
            .spawn(move || {
                let state = source();
                let saved_energy = state.initial_energy;
                let step = state.step;
                let engine = factory.create(state, params);
                let initial_energy =
                    saved_energy.or_else(|| Some(engine.diagnostics().total_energy()));
                Worker {
                    engine,
                    rx,
                    shared: worker_shared,
                    running: !start_paused,
                    max_rate: None,
                    initial_energy,
                    steps_per_second: 0.0,
                    last_step_ms: 0.0,
                    last_publish: None,
                    step,
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

    /// Arrête l'enregistrement (le fichier est clos par le thread de calcul).
    pub fn stop_recording(&self) {
        let _ = self.tx.send(Command::StopRecording);
    }

    /// État de l'enregistrement (instantanés écrits, octets, erreurs…).
    pub fn recording_status(&self) -> &RecordingStatus {
        &self.shared.recording
    }

    /// Corps de référence pour l'affichage relatif des traces.
    pub fn set_trace_reference(&self, id: Option<BodyId>) {
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
    steps_per_second: f64,
    last_step_ms: f64,
    last_publish: Option<Instant>,
    /// Numéro du pas courant (pour dater les points des traces).
    step: u64,
    recording: Option<ActiveRecording>,
}

/// Enregistrement en cours : un instantané tous les `interval` pas.
struct ActiveRecording {
    recorder: Recorder,
    interval: u64,
    next_step: u64,
    /// Devenirs accumulés depuis le dernier instantané.
    pending: Vec<(BodyId, BodyFate)>,
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
        };
        self.record_keyframe(&mut rec);
        self.recording = Some(rec);
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
        rec.next_step = self.step + rec.interval;
    }

    fn run_loop(&mut self) {
        self.publish();
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
                        self.publish();
                    }
                    Command::Resume => self.running = true,
                    Command::StepOnce => {
                        self.step();
                        self.publish();
                    }
                    Command::SetMaxRate(r) => self.max_rate = r.filter(|r| *r > 0.0),
                    Command::StartTraces(ids) => {
                        let mut set = TraceSet::new(ids);
                        let pos = self.engine.positions_of(&set.current_ids());
                        set.push(self.step, &pos);
                        *self.shared.traces.lock().expect("verrou empoisonné") = Some(set);
                        self.shared.tracing.store(true, Ordering::Release);
                    }
                    Command::SetTraceReference(id) => {
                        let mut guard = self.shared.traces.lock().expect("verrou empoisonné");
                        if let Some(set) = guard.as_mut() {
                            set.set_reference(id);
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
            if self
                .last_publish
                .map_or(true, |t| t.elapsed() >= PUBLISH_INTERVAL)
            {
                self.publish();
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

    fn publish(&mut self) {
        let mut state = SimulationState::default();
        self.engine.write_state(&mut state);
        state.initial_energy = self.initial_energy;
        let top = ranking::top_k(&state.bodies.ids, &state.bodies.masses, TOP_COUNT);
        let sequence = self.shared.sequence.load(Ordering::Relaxed) + 1;
        let snapshot = Arc::new(Snapshot {
            phase: Phase::Ready,
            state,
            diagnostics: self.engine.diagnostics(),
            top,
            steps_per_second: self.steps_per_second,
            last_step_ms: self.last_step_ms,
            running: self.running,
            sequence,
        });
        *self.shared.latest.lock().expect("verrou empoisonné") = snapshot;
        self.shared.sequence.store(sequence, Ordering::Release);
        self.last_publish = Some(Instant::now());
    }
}
