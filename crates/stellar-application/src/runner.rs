//! Pilotage d'une simulation dans un thread dédié, découplé de l'affichage.
//!
//! Le thread de simulation enchaîne les pas aussi vite que possible (ou à la
//! cadence maximale demandée) et publie régulièrement un [`Snapshot`]
//! immuable, que l'interface lit sans jamais bloquer le calcul.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stellar_domain::{
    ranking, BodyId, Diagnostics, EngineFactory, PhysicsEngine, SimulationConfig,
    SimulationState,
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
        });
        let worker_shared = Arc::clone(&shared);
        let params = config.physics;
        let thread = std::thread::Builder::new()
            .name("stellar-simulation".into())
            .spawn(move || {
                let state = source();
                let saved_energy = state.initial_energy;
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
                }
                .run();
            })
            .expect("impossible de lancer le thread de simulation");
        Self { config, tx, shared, thread: Some(thread) }
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
}

impl Worker {
    fn run(mut self) {
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
        let dt = t0.elapsed().as_secs_f64();
        self.last_step_ms = dt * 1e3;
        let inst = if dt > 0.0 { 1.0 / dt } else { 0.0 };
        self.steps_per_second = if self.steps_per_second == 0.0 {
            inst
        } else {
            0.9 * self.steps_per_second + 0.1 * inst
        };
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
