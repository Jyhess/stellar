//! Simulation without display: computation and automatic recording until a
//! target (step count, simulated time, real duration) or until a stop is
//! requested; at the end, the recording is closed and the final state saved.
//!
//! Used by the interface's monitoring screen and by the console mode.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use stellar_domain::SimulationConfig;

use crate::runner::{Phase, SimulationHandle};
use crate::service::{name_from_file, AppError, SimulatorService};

/// Stop condition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunTarget {
    /// Until a stop is requested.
    Manual,
    /// This number of steps.
    Steps(u64),
    /// This simulated time.
    SimTime(f64),
    /// This real computation time.
    WallTime(Duration),
}

/// Recording options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordSettings {
    pub interval: u32,
    pub dust_budget: Option<u64>,
}

/// Progress of a simulation without display.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub ready: bool,
    pub step: u64,
    pub time: f64,
    pub bodies: usize,
    pub steps_per_second: f64,
    pub elapsed: Duration,
    /// Share of the target reached (`None` without a target).
    pub fraction: Option<f64>,
    pub frames: u64,
    pub bytes: u64,
    /// The engine could not start: [`AppError::EngineFailed`].
    pub error: Option<AppError>,
}

/// Result of a simulation without display.
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
    /// Step and simulated time at the start.
    origin: (u64, f64),
}

/// Stop step of a target counted from step `from`.
fn stop_step(target: RunTarget, from: u64, dt: f64) -> Option<u64> {
    match target {
        RunTarget::Steps(n) => Some(from + n),
        RunTarget::SimTime(t) => Some(from + (t / dt.max(1e-300)).ceil().max(0.0) as u64),
        _ => None,
    }
}

impl HeadlessRun {
    /// Creates the simulation and starts recording (under the name `name`).
    pub fn start(
        service: &SimulatorService,
        config: SimulationConfig,
        name: &str,
        record: RecordSettings,
        target: RunTarget,
    ) -> Result<Self, AppError> {
        let dt = config.physics.dt;
        // Paused: stop step and recording armed before the first step.
        let handle = service.new_simulation_paused(config)?;
        handle.set_name(name);
        handle.stop_at(stop_step(target, 0, dt));
        service.start_recording(name, &handle, record.interval, record.dust_budget)?;
        handle.resume();
        Ok(Self {
            handle,
            target,
            started: Instant::now(),
            origin: (0, 0.0),
        })
    }

    /// Resumes the save `save`. Recording: `continue_with` if given (continued
    /// from the save's step), otherwise the one the save is linked to,
    /// otherwise a new recording `record`. Name: `name`, or the save's. The
    /// target is counted from the save's step. Also returns the continued
    /// recording.
    pub fn resume(
        service: &SimulatorService,
        save: &Path,
        continue_with: Option<&Path>,
        name: Option<&str>,
        record: RecordSettings,
        target: RunTarget,
    ) -> Result<(Self, Option<PathBuf>), AppError> {
        let data = service.read_save(save)?;
        let (step, time, dt) = (data.state.step, data.state.time, data.config.physics.dt);
        let loaded = service.start_loaded(data, continue_with)?;
        let handle = loaded.handle;
        let name = name.map(str::to_owned).unwrap_or_else(|| {
            name_from_file(&save.file_stem().unwrap_or_default().to_string_lossy())
        });
        handle.set_name(&name);
        handle.stop_at(stop_step(target, step, dt));
        let continued = match loaded.recording {
            Some(Ok(path)) => Some(path),
            Some(Err(e)) => {
                handle.shutdown();
                return Err(e.into());
            }
            None => {
                service.start_recording(&name, &handle, record.interval, record.dust_budget)?;
                None
            }
        };
        // A loaded save starts paused.
        handle.resume();
        Ok((
            Self {
                handle,
                target,
                started: Instant::now(),
                origin: (step, time),
            },
            continued,
        ))
    }

    /// Takes over a running simulation (sent to the background from the
    /// display), without a target: it runs until a stop is requested. A
    /// running recording continues.
    pub fn adopt(handle: SimulationHandle) -> Self {
        let snap = handle.latest();
        handle.stop_at(None);
        Self {
            origin: (snap.step, snap.time),
            handle,
            target: RunTarget::Manual,
            started: Instant::now(),
        }
    }

    pub fn handle(&self) -> &SimulationHandle {
        &self.handle
    }

    /// Hands the simulation over (to display it); the target is no longer watched.
    pub fn into_handle(self) -> SimulationHandle {
        self.handle.stop_at(None);
        self.handle
    }

    pub fn target(&self) -> RunTarget {
        self.target
    }

    /// Progress.
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
            error: (snap.phase == Phase::Failed).then(|| AppError::EngineFailed(snap.error.clone())),
        }
    }

    /// Is the target reached (or has the simulation failed)?
    pub fn done(&self) -> bool {
        let p = self.poll();
        if p.error.is_some() {
            return true;
        }
        let snap = self.handle.latest();
        match self.target {
            RunTarget::Manual => false,
            RunTarget::WallTime(d) => p.ready && p.elapsed >= d,
            // The computation paused itself at the stop step.
            RunTarget::Steps(_) | RunTarget::SimTime(_) => {
                p.ready && !snap.running && p.fraction.unwrap_or(0.0) >= 1.0
            }
        }
    }

    /// Stops the computation, closes the recording and saves the final state.
    pub fn finish(self, service: &SimulatorService) -> Result<RunOutcome, AppError> {
        let h = &self.handle;
        h.pause();
        // A stop during the preparation (generation, engine creation: long
        // with millions of bodies) waits for its end to save the initial state.
        while h.latest().phase == Phase::Initialising {
            std::thread::sleep(Duration::from_millis(20));
        }
        let first = h.latest();
        if first.phase == Phase::Failed {
            h.stop_recording();
            self.handle.shutdown();
            return Err(AppError::EngineFailed(first.error.clone()));
        }
        // Pausing publishes an up-to-date full state.
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
