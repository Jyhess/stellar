//! Simulation sans affichage : calcul et enregistrement automatique jusqu'à
//! une cible (nombre de pas, temps simulé, durée réelle) ou jusqu'à l'arrêt
//! demandé ; à la fin, l'enregistrement est clos et l'état final sauvegardé.
//!
//! Utilisée par l'écran de suivi de l'interface et par le mode console.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use stellar_domain::SimulationConfig;

use crate::runner::{Phase, SimulationHandle};
use crate::service::{AppError, SimulatorService};

/// Condition d'arrêt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunTarget {
    /// Jusqu'à l'arrêt demandé.
    Manual,
    /// Ce nombre de pas.
    Steps(u64),
    /// Ce temps simulé.
    SimTime(f64),
    /// Cette durée réelle de calcul.
    WallTime(Duration),
}

/// Options d'enregistrement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordSettings {
    pub interval: u32,
    pub dust_budget: Option<u64>,
}

/// Avancement d'une simulation sans affichage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub ready: bool,
    pub step: u64,
    pub time: f64,
    pub bodies: usize,
    pub steps_per_second: f64,
    pub elapsed: Duration,
    /// Part de la cible atteinte (`None` sans cible).
    pub fraction: Option<f64>,
    pub frames: u64,
    pub bytes: u64,
    pub error: Option<&'static str>,
}

/// Résultat d'une simulation sans affichage.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    pub save: PathBuf,
    pub recording: Option<PathBuf>,
    pub step: u64,
}

pub struct HeadlessRun {
    handle: SimulationHandle,
    target: RunTarget,
    started: Instant,
    /// Pas et temps simulé au départ.
    origin: (u64, f64),
}

impl HeadlessRun {
    /// Crée la simulation et démarre l'enregistrement (sous le nom `name`).
    pub fn start(
        service: &SimulatorService,
        config: SimulationConfig,
        name: &str,
        record: RecordSettings,
        target: RunTarget,
    ) -> Result<Self, AppError> {
        let dt = config.physics.dt.max(1e-300);
        let handle = service.new_simulation(config)?;
        handle.set_name(name);
        // Nouvelle simulation : départ au pas 0, au temps 0. Le pas d'arrêt
        // est transmis avant le premier pas.
        handle.stop_at(match target {
            RunTarget::Steps(n) => Some(n),
            RunTarget::SimTime(t) => Some((t / dt).ceil().max(0.0) as u64),
            _ => None,
        });
        service.start_recording(name, &handle, record.interval, record.dust_budget)?;
        Ok(Self {
            handle,
            target,
            started: Instant::now(),
            origin: (0, 0.0),
        })
    }

    pub fn handle(&self) -> &SimulationHandle {
        &self.handle
    }

    /// Rend la simulation (pour l'afficher) ; la cible n'est plus surveillée.
    pub fn into_handle(self) -> SimulationHandle {
        self.handle.stop_at(None);
        self.handle
    }

    pub fn target(&self) -> RunTarget {
        self.target
    }

    /// Avancement.
    pub fn poll(&self) -> Progress {
        let snap = self.handle.latest();
        let ready = snap.phase == Phase::Ready;
        let elapsed = self.started.elapsed();
        let (s0, t0) = self.origin;
        let fraction = match self.target {
            RunTarget::Manual => None,
            RunTarget::Steps(n) => Some(snap.step.saturating_sub(s0) as f64 / n.max(1) as f64),
            RunTarget::SimTime(t) => Some((snap.time - t0) / t.max(1e-300)),
            RunTarget::WallTime(d) => Some(elapsed.as_secs_f64() / d.as_secs_f64().max(1e-9)),
        };
        let status = self.handle.recording_status();
        Progress {
            ready,
            step: snap.step,
            time: snap.time,
            bodies: snap.body_count,
            steps_per_second: snap.steps_per_second,
            elapsed,
            fraction: fraction.map(|f| f.clamp(0.0, 1.0)),
            frames: status.frames.load(Ordering::Relaxed),
            bytes: status.bytes.load(Ordering::Relaxed),
            error: (snap.phase == Phase::Failed).then_some("le moteur de calcul n'a pas pu démarrer"),
        }
    }

    /// La cible est-elle atteinte (ou la simulation en échec) ?
    pub fn done(&self) -> bool {
        let p = self.poll();
        if p.error.is_some() {
            return true;
        }
        let snap = self.handle.latest();
        match self.target {
            RunTarget::Manual => false,
            RunTarget::WallTime(d) => p.ready && p.elapsed >= d,
            // Le calcul s'est mis en pause au pas d'arrêt.
            RunTarget::Steps(_) | RunTarget::SimTime(_) => {
                p.ready && !snap.running && p.fraction.unwrap_or(0.0) >= 1.0
            }
        }
    }

    /// Arrête le calcul, clôt l'enregistrement et sauvegarde l'état final.
    pub fn finish(self, service: &SimulatorService) -> Result<RunOutcome, AppError> {
        let h = &self.handle;
        h.pause();
        // La pause publie un état complet à jour.
        let deadline = Instant::now() + Duration::from_secs(60);
        let snap = loop {
            let s = h.latest();
            if (!s.running && s.state.step == s.step) || Instant::now() > deadline {
                break s;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let recording = h
            .recording_status()
            .current_path
            .lock()
            .ok()
            .and_then(|p| p.clone());
        h.stop_recording();
        let save = service.save(&h.name(), h.config(), &snap, None, None)?;
        let step = snap.step;
        self.handle.shutdown();
        Ok(RunOutcome {
            save,
            recording,
            step,
        })
    }
}
