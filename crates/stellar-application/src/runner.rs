//! Driving a simulation on a dedicated thread, decoupled from the display.
//!
//! The simulation thread chains steps as fast as possible (or at the
//! requested maximum rate) and regularly publishes an immutable
//! [`Snapshot`], which the interface reads without ever blocking the
//! computation.
//!
//! When the engine offers direct access to its data for display
//! ([`PhysicsEngine::live_view`]), the expensive full copy of the state is
//! made only every one to two seconds; in between, light publications
//! carry only the global quantities and the positions of the watched bodies
//! ([`SimulationHandle::set_watched`]).

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
use crate::trace::{cluster_members, TraceReference, TraceSet};
use stellar_domain::{
    ranking, PhysicsParams, BodyId, Diagnostics, EjectedCluster, EngineError, EngineFactory, SystemSummary, PhysicsEngine, SimulationConfig, SimulationState,
    Vec3,
};

/// Number of bodies listed in the published ranking.
pub const TOP_COUNT: usize = 10;

/// Minimum interval between two publications (≈ 60 Hz).
const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);

/// Interval between two full copies of the state when the display reads
/// the engine's data directly: one second per three million bodies, between
/// one and two seconds.
fn full_state_interval(bodies: usize) -> Duration {
    Duration::from_secs_f64((bodies as f64 / 3e6).clamp(1.0, 2.0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Generating the initial state / building the engine.
    Initialising,
    Ready,
    /// The engine could not start (message in [`Snapshot::error`]).
    Failed,
}

/// Snapshot published by the simulation thread.
///
/// `state` (and the quantities derived from it: `top`, `total_mass`) may be
/// a few steps old when the engine is displayed directly; `step`, `time`,
/// `body_count`, `diagnostics`, `center_of_mass` and `watched` are always
/// up to date.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub phase: Phase,
    /// Last full copy of the state (shared between publications).
    pub state: Arc<SimulationState>,
    /// Current step.
    pub step: u64,
    /// Current simulated time.
    pub time: f64,
    /// Current body count.
    pub body_count: usize,
    pub diagnostics: Diagnostics,
    /// Current center of mass, if the engine provides it cheaply.
    pub center_of_mass: Option<Vec3>,
    /// Current positions of the watched bodies (missing: removed or merged).
    pub watched: Vec<(BodyId, Vec3)>,
    /// Sub-simulation (its marker body) holding each watched body found.
    pub watched_systems: Vec<(BodyId, BodyId)>,
    /// Ranking of the most massive bodies: (id, mass).
    pub top: Vec<(BodyId, f64)>,
    /// Total mass of the bodies present in the simulation.
    pub total_mass: f64,
    /// Kept ejected clusters, described at the last cleanup.
    pub clusters: Arc<Vec<EjectedCluster>>,
    /// Step of that cleanup (`None`: none since the start).
    pub clusters_step: Option<u64>,
    /// Subsystems computed separately (empty or a single one: a simulation in
    /// one piece).
    pub systems: Vec<SystemSummary>,
    /// Computation rate (moving average).
    pub steps_per_second: f64,
    /// Duration of the last step, in milliseconds.
    pub last_step_ms: f64,
    pub running: bool,
    /// Publication number, strictly increasing.
    pub sequence: u64,
    /// Why the engine could not start.
    pub error: Option<EngineError>,
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
            watched_systems: Vec::new(),
            top: Vec::new(),
            total_mass: 0.0,
            clusters: Arc::default(),
            clusters_step: None,
            systems: Vec::new(),
            steps_per_second: 0.0,
            last_step_ms: 0.0,
            running,
            sequence: 0,
            error: None,
        }
    }

    /// Mass that left the simulation (bodies ejected, then removed).
    pub fn ejected_mass(&self) -> Option<f64> {
        let m0 = self.state.initial_mass?;
        (self.phase == Phase::Ready).then(|| (m0 - self.total_mass).max(0.0))
    }

    /// Share of the present mass carried by a body of mass `m`.
    pub fn mass_fraction(&self, m: f64) -> f64 {
        if self.total_mass > 0.0 {
            m / self.total_mass
        } else {
            0.0
        }
    }

    /// Current position of a watched body ([`SimulationHandle::set_watched`]).
    pub fn watched_position(&self, id: BodyId) -> Option<Vec3> {
        self.watched.iter().find(|(w, _)| *w == id).map(|(_, p)| *p)
    }

    /// Current position of a body: watched if possible, otherwise taken from
    /// the last full copy of the state (linear search).
    pub fn position_of(&self, id: BodyId) -> Option<Vec3> {
        self.watched_position(id).or_else(|| {
            let b = &self.state.bodies;
            b.index_of(id).map(|i| b.positions[i])
        })
    }

    /// Relative change of the total energy since the start.
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
    StopAt(Option<u64>),
    StartTraces(Vec<BodyId>),
    SetWatched(Vec<BodyId>),
    /// Removes the negligible lone bodies now.
    PruneNegligible,
    SetAutoPrune(bool),
    StopTraces,
    SetTraceReference(Option<TraceReference>),
    StartRecording {
        writer: Box<dyn RecordingWriter>,
        interval: u32,
        dust_budget: Option<u64>,
    },
    StopRecording,
    /// Closes the recording and returns what is needed to continue it, with
    /// the exact state of the simulation at the same moment.
    SuspendRecording(Sender<Option<(RecordingLink, SimulationState)>>),
    ResumeRecording {
        writer: Box<dyn RecordingWriter>,
        link: RecordingLink,
    },
    Shutdown,
}

/// Fate of a body that disappeared.
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
    /// Automatic removal of the negligible lone bodies.
    auto_prune: AtomicBool,
    recording: Arc<RecordingStatus>,
    /// Direct access of the engine for display, once the engine is created.
    live_view: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
}

/// Handle driving a running simulation. Stopping (or dropping) it ends the
/// computation thread cleanly.
pub struct SimulationHandle {
    config: SimulationConfig,
    /// Name of the simulation: name of its saves (F5) and recordings.
    name: Mutex<String>,
    tx: Sender<Command>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SimulationHandle {
    /// Starts a simulation. `source` produces the initial state; it is called
    /// on the simulation thread so as not to freeze the interface.
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
            auto_prune: AtomicBool::new(false),
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
                // Older saves: initial mass derived from the initial conditions.
                let initial_mass = state.initial_mass.or(Some(initial_count as f64));
                let step = state.step;
                let engine = match crate::MultiSystem::new(factory, state, params).map(Box::new) {
                    Ok(e) => e,
                    Err(e) => {
                        let mut s = Snapshot::initialising(false);
                        s.phase = Phase::Failed;
                        s.error = Some(e);
                        s.sequence = 1;
                        *worker_shared.latest.lock().expect("poisoned lock") = Arc::new(s);
                        worker_shared.sequence.store(1, Ordering::Release);
                        // Wait for the stop requested by the interface.
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
                *worker_shared.live_view.lock().expect("poisoned lock") = live_view;
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
                    clusters: Arc::default(),
                    clusters_step: None,
                    recent: VecDeque::new(),
                    spare: None,
                    step,
                    dt: params.dt,
                    params,
                    stop_at: None,
                    recording: None,
                }
                .run();
            })
            .expect("cannot start the simulation thread");
        Self {
            config,
            name: Mutex::new("simulation".to_owned()),
            tx,
            shared,
            thread: Some(thread),
        }
    }

    pub fn name(&self) -> String {
        self.name.lock().expect("poisoned lock").clone()
    }

    pub fn set_name(&self, name: &str) {
        *self.name.lock().expect("poisoned lock") = name.to_owned();
    }

    pub fn config(&self) -> &SimulationConfig {
        &self.config
    }

    /// Last published snapshot (a read that never blocks the computation).
    pub fn latest(&self) -> Arc<Snapshot> {
        Arc::clone(&self.shared.latest.lock().expect("poisoned lock"))
    }

    /// Direct access of the engine for display (`None` until the engine is
    /// created, or if it offers none). See [`PhysicsEngine::live_view`].
    pub fn live_view(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.shared
            .live_view
            .lock()
            .expect("poisoned lock")
            .clone()
    }

    /// Bodies whose position is published in every snapshot
    /// ([`Snapshot::watched`]), typically the selected body.
    pub fn set_watched(&self, ids: Vec<BodyId>) {
        let _ = self.tx.send(Command::SetWatched(ids));
    }

    /// Removes now the sub-simulations reduced to a single body escaping with
    /// a negligible influence ([`crate::MultiSystem::prune_negligible`]).
    pub fn prune_negligible(&self) {
        let _ = self.tx.send(Command::PruneNegligible);
    }

    /// Does the same at every cleanup.
    pub fn set_auto_prune(&self, on: bool) {
        self.shared.auto_prune.store(on, Ordering::Release);
        let _ = self.tx.send(Command::SetAutoPrune(on));
    }

    /// Is the automatic removal of negligible lone bodies on?
    pub fn auto_prune(&self) -> bool {
        self.shared.auto_prune.load(Ordering::Acquire)
    }

    /// Number of the last published snapshot.
    pub fn sequence(&self) -> u64 {
        self.shared.sequence.load(Ordering::Acquire)
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }

    pub fn resume(&self) {
        let _ = self.tx.send(Command::Resume);
    }

    /// Performs a single step (useful while paused).
    pub fn step_once(&self) {
        let _ = self.tx.send(Command::StepOnce);
    }

    /// Caps the computation rate (steps per second); `None` = unlimited.
    pub fn set_max_rate(&self, rate: Option<f64>) {
        let _ = self.tx.send(Command::SetMaxRate(rate));
    }

    /// Pauses the computation once step `step` is reached (`None`: never).
    pub fn stop_at(&self, step: Option<u64>) {
        let _ = self.tx.send(Command::StopAt(step));
    }

    /// Starts recording the traces of the given bodies (one point per step,
    /// from now on). Replaces the existing traces.
    pub fn start_traces(&self, ids: Vec<BodyId>) {
        let _ = self.tx.send(Command::StartTraces(ids));
    }

    /// Starts recording: a snapshot now, then every `interval` steps. Replaces
    /// any running recording.
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

    /// Suspends the running recording (file closed cleanly) and returns the
    /// resume link along with the state of the simulation at the same step.
    /// `None` if no recording is active. Blocking.
    pub fn suspend_recording(&self) -> Option<(RecordingLink, SimulationState)> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Command::SuspendRecording(tx)).ok()?;
        rx.recv().ok().flatten()
    }

    /// Continues a suspended recording.
    pub fn resume_recording(&self, writer: Box<dyn RecordingWriter>, link: RecordingLink) {
        let _ = self.tx.send(Command::ResumeRecording { writer, link });
    }

    /// True if a recording is running.
    pub fn is_recording(&self) -> bool {
        self.shared.recording.active.load(Ordering::Acquire)
    }

    /// Stops recording (the file is closed by the computation thread).
    pub fn stop_recording(&self) {
        let _ = self.tx.send(Command::StopRecording);
    }

    /// State of the recording (snapshots written, bytes, errors…).
    pub fn recording_status(&self) -> &RecordingStatus {
        &self.shared.recording
    }

    /// Reference frame of the relative display of traces (body, barycenter or
    /// local cluster).
    pub fn set_trace_reference(&self, id: Option<TraceReference>) {
        let _ = self.tx.send(Command::SetTraceReference(id));
    }

    /// Stops and clears the traces.
    pub fn stop_traces(&self) {
        let _ = self.tx.send(Command::StopTraces);
    }

    /// Read access to the traces (short lock: copy what is needed).
    pub fn traces(&self) -> MutexGuard<'_, Option<TraceSet>> {
        self.shared.traces.lock().expect("poisoned lock")
    }

    /// Current identifier of a body, following the chain of mergers. `None` if
    /// the body was removed (ejection).
    pub fn resolve(&self, id: BodyId) -> Option<BodyId> {
        let fates = self.shared.fates.lock().expect("poisoned lock");
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

    /// Stops the simulation thread and waits for it to end.
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
    engine: Box<crate::MultiSystem>,
    rx: Receiver<Command>,
    shared: Arc<Shared>,
    running: bool,
    max_rate: Option<f64>,
    initial_energy: Option<f64>,
    initial_mass: Option<f64>,
    steps_per_second: f64,
    last_step_ms: f64,
    last_publish: Option<Instant>,
    /// Last full copy of the state.
    last_full: Option<Instant>,
    /// The display reads the engine's data directly: spaced full copies, light
    /// publications in between.
    direct_view: bool,
    /// Bodies whose position comes with every publication.
    watched: Vec<BodyId>,
    /// Last published full copy, and the quantities derived from it.
    full: Arc<SimulationState>,
    top: Vec<(BodyId, f64)>,
    total_mass: f64,
    clusters: Arc<Vec<EjectedCluster>>,
    clusters_step: Option<u64>,
    /// Recent copies, whose arrays are recycled once the interface has let them
    /// go (avoids reallocating hundreds of megabytes).
    recent: VecDeque<Arc<SimulationState>>,
    spare: Option<SimulationState>,
    /// Number of the current step (to date the points of traces).
    step: u64,
    /// Time step (fixed).
    dt: f64,
    /// Physical parameters (search for the local cluster of traces).
    params: PhysicsParams,
    /// Step at which to pause ([`SimulationHandle::stop_at`]).
    stop_at: Option<u64>,
    recording: Option<ActiveRecording>,
}

/// Running recording: a snapshot every `interval` steps.
struct ActiveRecording {
    recorder: Recorder,
    interval: u64,
    next_step: u64,
    /// Fates accumulated since the last snapshot.
    pending: Vec<(BodyId, BodyFate)>,
    dust_budget: Option<u64>,
    last_frame_step: u64,
}

impl Worker {
    fn run(mut self) {
        self.run_loop();
        // End of the thread: a running recording is closed cleanly.
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
        *status.error.lock().expect("poisoned lock") = None;
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
        *status.error.lock().expect("poisoned lock") = None;
        let recorder = Recorder::start(writer, Arc::clone(status), link.dust_budget);
        let mut rec = ActiveRecording {
            recorder,
            interval: link.interval.max(1) as u64,
            next_step: link.next_step,
            pending: link.pending,
            dust_budget: link.dust_budget,
            last_frame_step: link.last_frame_step,
        };
        // Continuing a recording from an unlinked save: a snapshot is written
        // immediately, at the save's step.
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
        let mut groups = Vec::new();
        if s.systems.len() > 1 {
            groups = vec![0u16; s.bodies.len()];
            for (k, span) in s.systems.iter().enumerate() {
                let end = (span.start + span.len).min(groups.len());
                for g in &mut groups[span.start.min(end)..end] {
                    *g = k.min(u16::MAX as usize) as u16;
                }
            }
        }
        rec.recorder.push(Keyframe {
            step: self.step,
            time: s.time,
            bodies: s.bodies,
            fates: std::mem::take(&mut rec.pending),
            dust_weight: 1.0,
            groups,
        });
        rec.last_frame_step = self.step;
        rec.next_step = self.step + rec.interval;
    }

    fn run_loop(&mut self) {
        self.publish(true);
        loop {
            // 1. Handling of commands (blocking while paused).
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
                    Command::PruneNegligible => {
                        let report = self.engine.prune_negligible();
                        if !report.removed.is_empty() {
                            self.apply_fates(&[], &report.removed);
                            self.publish(true);
                        }
                    }
                    Command::SetAutoPrune(on) => self.engine.set_auto_prune(on),
                    Command::SetMaxRate(r) => self.max_rate = r.filter(|r| *r > 0.0),
                    Command::StopAt(step) => self.stop_at = step,
                    Command::StartTraces(ids) => {
                        let mut set = TraceSet::new(ids);
                        let pos = self.engine.positions_of(&set.current_ids());
                        set.push(self.step, &pos);
                        if let Some(c) = self.engine.center_of_mass() {
                            set.push_barycenter(self.step, c);
                        }
                        *self.shared.traces.lock().expect("poisoned lock") = Some(set);
                        self.shared.tracing.store(true, Ordering::Release);
                    }
                    Command::SetTraceReference(id) => {
                        let mut guard = self.shared.traces.lock().expect("poisoned lock");
                        if let Some(set) = guard.as_mut() {
                            set.set_reference_kind(id);
                            self.update_cluster(set);
                            let ids = set.current_ids();
                            let pos = self.engine.positions_of(&ids);
                            set.push(self.step, &pos);
                            self.push_system_center(set);
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
                        *self.shared.traces.lock().expect("poisoned lock") = None;
                    }
                    Command::Shutdown => return,
                }
            }

            // 2. One computation step.
            let started = Instant::now();
            self.step();
            if self.stop_at.is_some_and(|s| self.step >= s) {
                self.stop_at = None;
                self.running = false;
                self.publish(true);
                continue;
            }
            let n = self.engine.body_count();
            if self.direct_view {
                // Live view: full copy every one to two seconds, light
                // publications in between.
                let full = self
                    .last_full
                    .is_none_or(|t| t.elapsed() >= full_state_interval(n));
                if full
                    || self
                        .last_publish
                        .is_none_or(|t| t.elapsed() >= PUBLISH_INTERVAL)
                {
                    self.publish(full);
                }
            } else {
                // Beyond a million bodies, copying the state becomes
                // expensive: publications are spaced out.
                let scale = (n as f64 / 1e6).max(1.0);
                if self
                    .last_publish
                    .is_none_or(|t| t.elapsed() >= PUBLISH_INTERVAL.mul_f64(scale))
                {
                    self.publish(true);
                }
            }

            // 3. Rate limiting, if any.
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
        let mut report = self.engine.step();
        self.step += 1;
        if let Some(clusters) = report.clusters.take() {
            self.clusters = Arc::new(clusters);
            self.clusters_step = Some(self.step);
        }
        let dt = t0.elapsed().as_secs_f64();
        self.last_step_ms = dt * 1e3;
        let inst = if dt > 0.0 { 1.0 / dt } else { 0.0 };
        self.steps_per_second = if self.steps_per_second == 0.0 {
            inst
        } else {
            0.9 * self.steps_per_second + 0.1 * inst
        };
        self.apply_fates(&report.merges, &report.removed);
        if self.shared.tracing.load(Ordering::Acquire) {
            let mut guard = self.shared.traces.lock().expect("poisoned lock");
            if let Some(set) = guard.as_mut() {
                let ids = set.current_ids();
                if !ids.is_empty() {
                    let pos = self.engine.positions_of(&ids);
                    set.push(self.step, &pos);
                }
                if let Some(c) = self.engine.center_of_mass() {
                    set.push_barycenter(self.step, c);
                }
                self.push_system_center(set);
            }
        }
        if let Some(mut rec) = self.recording.take() {
            if self.step >= rec.next_step {
                self.record_keyframe(&mut rec);
            }
            self.recording = Some(rec);
        }
    }

    /// Passes on the bodies that disappeared (mergers: absorbed → survivor)
    /// to the traces, the recording and the identifier resolution.
    fn apply_fates(&mut self, merges: &[(BodyId, BodyId)], removed: &[BodyId]) {
        if merges.is_empty() && removed.is_empty() {
            return;
        }
        if self.shared.tracing.load(Ordering::Acquire) {
            if let Some(set) = self.shared.traces.lock().expect("poisoned lock").as_mut() {
                set.apply_fates(merges, removed);
            }
        }
        if let Some(rec) = &mut self.recording {
            rec.pending
                .extend(merges.iter().map(|&(a, s)| (a, BodyFate::MergedInto(s))));
            rec.pending
                .extend(removed.iter().map(|&id| (id, BodyFate::Removed)));
        }
        let mut fates = self.shared.fates.lock().expect("poisoned lock");
        for &(absorbed, survivor) in merges {
            fates.insert(absorbed, Fate::MergedInto(survivor));
        }
        for &id in removed {
            fates.insert(id, Fate::Removed);
        }
    }

    /// Full copy of the state, into the arrays of a copy nobody uses any more
    /// when possible.
    fn refresh_full_state(&mut self) {
        // The copy published last is still displayed; the previous ones have
        // normally been released by the interface.
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
        if self.shared.tracing.load(Ordering::Acquire) {
            let shared = Arc::clone(&self.shared);
            let mut guard = shared.traces.lock().expect("poisoned lock");
            if let Some(set) = guard.as_mut() {
                self.update_cluster(set);
            }
        }
    }

    /// Adds the current barycenter of the sub-simulation used as reference
    /// frame for the traces (none if it no longer exists).
    fn push_system_center(&self, set: &mut TraceSet) {
        if let Some(anchor) = set.system_anchor() {
            if let Some(s) = self.engine.systems().into_iter().find(|s| s.anchor == anchor) {
                set.push_system(self.step, s.center);
            }
        }
    }

    /// Picks again the members of the local cluster used as reference frame
    /// for the traces, from the last full copy of the state.
    fn update_cluster(&self, set: &mut TraceSet) {
        if let Some(around) = set.cluster_around() {
            if let Some(members) = cluster_members(&self.full.bodies, around, &self.params) {
                set.set_cluster_members(members);
            }
        }
    }

    /// Publishes a snapshot; `full`: with a new full copy of the state
    /// (otherwise, the previous one is reused).
    fn publish(&mut self, full: bool) {
        if full {
            self.refresh_full_state();
        }
        let found: Vec<(BodyId, (Vec3, BodyId))> = if self.watched.is_empty() {
            Vec::new()
        } else {
            let pos = self.engine.locate(&self.watched);
            self.watched
                .iter()
                .zip(pos)
                .filter_map(|(&id, p)| p.map(|p| (id, p)))
                .collect()
        };
        let watched = found.iter().map(|&(id, (p, _))| (id, p)).collect();
        let watched_systems = found.iter().map(|&(id, (_, s))| (id, s)).collect();
        // Without live view, the interface derives it from the full state.
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
            // Fixed time step: current time derived from the last copy.
            time: self.full.time + (self.step as f64 - self.full.step as f64) * self.dt,
            body_count: self.engine.body_count(),
            diagnostics: self.engine.diagnostics(),
            center_of_mass,
            watched,
            watched_systems,
            top: self.top.clone(),
            total_mass: self.total_mass,
            clusters: Arc::clone(&self.clusters),
            clusters_step: self.clusters_step,
            systems: self.engine.systems(),
            steps_per_second: self.steps_per_second,
            last_step_ms: self.last_step_ms,
            running: self.running,
            sequence,
            error: None,
        });
        *self.shared.latest.lock().expect("poisoned lock") = snapshot;
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

    /// Fake engine: uniform translation, body 2 absorbed at step 3.
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
        ) -> Result<Box<dyn PhysicsEngine>, stellar_domain::EngineError> {
            Ok(Box::new(Drift { state }))
        }
    }

    /// The same engine, but declaring itself directly displayed.
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
        ) -> Result<Box<dyn PhysicsEngine>, stellar_domain::EngineError> {
            Ok(Box::new(DirectDrift(Drift { state })))
        }
    }

    /// Live view: the full state is copied only every one to two seconds, but
    /// the step, the center of mass and the watched bodies are published up to
    /// date.
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
        // Initial full copy (step 0), not renewed yet.
        assert_eq!(snap.state.step, 0);
        assert!(snap.step >= 50);
        assert_eq!(snap.body_count, 1);
        // Body 1 moves one unit per step; it has absorbed body 2.
        let p = snap.watched_position(1).expect("tracked body");
        assert_eq!(p.x, snap.step as f64);
        assert!(snap.center_of_mass.is_some());
        handle.pause();
        // Pausing publishes an up-to-date full copy.
        for _ in 0..500 {
            let s = handle.latest();
            if !s.running && s.state.step == s.step {
                handle.shutdown();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no full copy on pause");
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
        panic!("step {step} never reached");
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
        let (link, saved) = handle.suspend_recording().expect("active recording");
        assert!(!handle.is_recording());
        let steps: Vec<u64> = mem.0.lock().unwrap().iter().map(|f| f.step).collect();
        assert_eq!(steps, vec![0, 2, 4]);
        assert_eq!(saved.step, 5);
        assert_eq!(link.last_frame_step, 4);
        assert_eq!(link.next_step, 6);
        assert_eq!(link.interval, 2);
        assert_eq!(saved.initial_mass, Some(2.0));
        // The merger of step 3 appears in the snapshot of step 4.
        assert_eq!(
            mem.0.lock().unwrap()[2].fates,
            vec![(2, BodyFate::MergedInto(1))]
        );
        handle.shutdown();

        // Resume from the save: the next snapshot is at step 6.
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
