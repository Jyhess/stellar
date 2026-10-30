//! Playback of a recording at display rate.
//!
//! Between two snapshots A and B, each body follows a **cubic Hermite
//! interpolation** built on its positions and velocities at both ends: the
//! trajectory is smooth and matches the recorded velocities. A body
//! absorbed between A and B joins the body that absorbed it; a removed body
//! continues in a straight line, then disappears.
//!
//! Snapshots are read and decoded by a dedicated thread, with a cache and
//! read-ahead in the playback direction. If a snapshot is not ready yet,
//! the clock waits instead of jumping.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use rayon::prelude::*;
use stellar_domain::{ranking, BodyId, BodySet, SimulationState, Vec3};

use crate::recording::{BodyFate, Keyframe, RecordingInfo, RecordingReader};

/// Number of snapshots kept in memory.
const CACHE_FRAMES: usize = 8;
/// Snapshots read ahead in the playback direction.
const PREFETCH: usize = 3;

// ----------------------------------------------------------------------
// Clock
// ----------------------------------------------------------------------

/// Playback position, in snapshots (a real number between 0 and n − 1).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackClock {
    pub position: f64,
    /// Speed in snapshots per second (always positive).
    pub speed: f64,
    pub reverse: bool,
    pub playing: bool,
    frame_count: usize,
}

impl PlaybackClock {
    pub fn new(frame_count: usize) -> Self {
        Self {
            position: 0.0,
            speed: 10.0,
            reverse: false,
            playing: false,
            frame_count,
        }
    }

    pub fn last(&self) -> f64 {
        self.frame_count.saturating_sub(1) as f64
    }

    /// Advances by `dt` seconds; stops at both ends.
    pub fn advance(&mut self, dt: f64) {
        if !self.playing {
            return;
        }
        let delta = self.speed * dt * if self.reverse { -1.0 } else { 1.0 };
        let p = self.position + delta;
        if p >= self.last() || p <= 0.0 {
            self.playing = false;
        }
        self.position = p.clamp(0.0, self.last());
    }

    pub fn seek(&mut self, position: f64) {
        self.position = position.clamp(0.0, self.last());
    }

    /// Jumps to the next (`+1`) or previous (`-1`) whole snapshot.
    pub fn step(&mut self, direction: i32) {
        let p = if direction > 0 {
            (self.position + 1e-9).floor() + 1.0
        } else {
            (self.position - 1e-9).ceil() - 1.0
        };
        self.playing = false;
        self.seek(p);
    }

    /// Surrounding snapshots and fraction: (a, b, t), with b = a + 1.
    pub fn pair(&self) -> (usize, usize, f64) {
        if self.frame_count < 2 {
            return (0, 0, 0.0);
        }
        let a = (self.position.floor() as usize).min(self.frame_count - 2);
        (a, a + 1, (self.position - a as f64).clamp(0.0, 1.0))
    }
}

// ----------------------------------------------------------------------
// Interpolation
// ----------------------------------------------------------------------

/// Where a body of A ends up in B.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The same body, at the given index of B.
    Same(u32),
    /// Absorbed by the body at the given index of B.
    Merged(u32),
    /// Removed (or not found): straight-line extrapolation.
    Gone,
}

/// Maps each body of A to where it ends up in B. Bodies of both snapshots
/// are sorted by identifier; chained mergers between A and B are followed
/// through the fates recorded in B.
pub fn build_targets(a: &Keyframe, b: &Keyframe) -> Vec<Target> {
    let fates: HashMap<BodyId, BodyFate> = b.fates.iter().copied().collect();
    let index_in_b = |id: BodyId| b.bodies.ids.binary_search(&id).ok().map(|i| i as u32);
    a.bodies
        .ids
        .par_iter()
        .map(|&id| {
            if let Some(i) = index_in_b(id) {
                return Target::Same(i);
            }
            let mut cur = id;
            for _ in 0..64 {
                match fates.get(&cur) {
                    Some(BodyFate::MergedInto(s)) => {
                        if let Some(i) = index_in_b(*s) {
                            return Target::Merged(i);
                        }
                        cur = *s;
                    }
                    _ => break,
                }
            }
            Target::Gone
        })
        .collect()
}

/// Interpolates between A and B at fraction `t` ∈ [0, 1]; writes the result
/// into `out` (same bodies as A, masses of A).
pub fn interpolate(a: &Keyframe, b: &Keyframe, targets: &[Target], t: f64, out: &mut BodySet) {
    let n = a.bodies.len();
    let span = (b.time - a.time).max(0.0);
    let (t2, t3) = (t * t, t * t * t);
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    // Derivatives (with respect to physical time).
    let inv = if span > 0.0 { 1.0 / span } else { 0.0 };
    let d00 = (6.0 * t2 - 6.0 * t) * inv;
    let d10 = 3.0 * t2 - 4.0 * t + 1.0;
    let d01 = (-6.0 * t2 + 6.0 * t) * inv;
    let d11 = 3.0 * t2 - 2.0 * t;

    out.ids.clear();
    out.ids.extend_from_slice(&a.bodies.ids);
    out.masses.clear();
    out.masses.extend_from_slice(&a.bodies.masses);
    out.positions.resize(n, Vec3::ZERO);
    out.velocities.resize(n, Vec3::ZERO);

    out.positions
        .par_iter_mut()
        .zip(out.velocities.par_iter_mut())
        .enumerate()
        .for_each(|(i, (p, v))| {
            let p0 = a.bodies.positions[i];
            let v0 = a.bodies.velocities[i];
            let end = match targets.get(i).copied().unwrap_or(Target::Gone) {
                Target::Same(j) | Target::Merged(j) => {
                    let j = j as usize;
                    Some((b.bodies.positions[j], b.bodies.velocities[j]))
                }
                Target::Gone => None,
            };
            match end {
                Some((p1, v1)) if span > 0.0 => {
                    *p = p0 * h00 + v0 * (h10 * span) + p1 * h01 + v1 * (h11 * span);
                    *v = p0 * d00 + v0 * d10 + p1 * d01 + v1 * d11;
                }
                Some((p1, _)) => {
                    *p = p0.lerp(p1, t);
                    *v = v0;
                }
                None => {
                    *p = p0 + v0 * (span * t);
                    *v = v0;
                }
            }
        });
}

// ----------------------------------------------------------------------
// Background loading
// ----------------------------------------------------------------------

#[derive(Default)]
struct Cache {
    frames: HashMap<usize, Arc<Keyframe>>,
    pending: HashSet<usize>,
    focus: usize,
    error: Option<String>,
}

struct Loader {
    cache: Arc<Mutex<Cache>>,
    tx: Option<Sender<usize>>,
    thread: Option<JoinHandle<()>>,
}

impl Loader {
    fn start(mut reader: Box<dyn RecordingReader>) -> Self {
        let cache = Arc::new(Mutex::new(Cache::default()));
        let (tx, rx): (Sender<usize>, Receiver<usize>) = mpsc::channel();
        let c = Arc::clone(&cache);
        let thread = std::thread::Builder::new()
            .name("stellar-replay-loader".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    // Accumulated requests are handled from the most recent (the most urgent)
                    // to the oldest.
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    for &i in batch.iter().rev() {
                        if c.lock().expect("verrou").frames.contains_key(&i) {
                            continue;
                        }
                        let result = reader.read_frame(i);
                        let mut g = c.lock().expect("verrou");
                        g.pending.remove(&i);
                        match result {
                            Ok(f) => {
                                g.frames.insert(i, Arc::new(f));
                                // Eviction of the farthest snapshots.
                                while g.frames.len() > CACHE_FRAMES {
                                    let focus = g.focus;
                                    let far = *g
                                        .frames
                                        .keys()
                                        .max_by_key(|&&k| k.abs_diff(focus))
                                        .expect("cache non vide");
                                    g.frames.remove(&far);
                                }
                            }
                            Err(e) => g.error = Some(e.to_string()),
                        }
                    }
                }
            })
            .expect("impossible de lancer le thread de relecture");
        Self {
            cache,
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    /// Returns the snapshot if it is ready; otherwise requests it.
    fn get(&self, i: usize) -> Option<Arc<Keyframe>> {
        let mut g = self.cache.lock().expect("verrou");
        if let Some(f) = g.frames.get(&i) {
            return Some(Arc::clone(f));
        }
        if g.pending.insert(i) {
            if let Some(tx) = &self.tx {
                let _ = tx.send(i);
            }
        }
        None
    }

    fn prefetch(&self, i: usize) {
        let _ = self.get(i);
    }

    fn set_focus(&self, i: usize) {
        self.cache.lock().expect("verrou").focus = i;
    }

    fn error(&self) -> Option<String> {
        self.cache.lock().expect("verrou").error.clone()
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ----------------------------------------------------------------------
// Player
// ----------------------------------------------------------------------

struct Pair {
    a: usize,
    b: usize,
    fa: Arc<Keyframe>,
    fb: Arc<Keyframe>,
    targets: Vec<Target>,
    top: Vec<(BodyId, f64)>,
}

/// Recording player: clock, loading and interpolation.
pub struct ReplayPlayer {
    info: RecordingInfo,
    loader: Loader,
    pub clock: PlaybackClock,
    fates: HashMap<BodyId, BodyFate>,
    pair: Option<Pair>,
    state: SimulationState,
    /// Position actually displayed (may lag behind the clock while a snapshot
    /// is awaited).
    shown: Option<f64>,
    fraction: f64,
}

impl ReplayPlayer {
    pub fn new(reader: Box<dyn RecordingReader>) -> Self {
        let info = reader.info().clone();
        let clock = PlaybackClock::new(info.frames.len());
        Self {
            info,
            loader: Loader::start(reader),
            clock,
            fates: HashMap::new(),
            pair: None,
            state: SimulationState::default(),
            shown: None,
            fraction: 0.0,
        }
    }

    pub fn info(&self) -> &RecordingInfo {
        &self.info
    }

    pub fn frame_count(&self) -> usize {
        self.info.frames.len()
    }

    /// Interpolated state at the displayed position.
    pub fn state(&self) -> &SimulationState {
        &self.state
    }

    /// Swaps the interpolated state for `replacement` (a recycled buffer):
    /// avoids a full copy at every frame. The returned buffer is entirely
    /// rewritten at the next interpolation.
    pub fn swap_state(&mut self, replacement: SimulationState) -> SimulationState {
        std::mem::replace(&mut self.state, replacement)
    }

    /// The most massive bodies of the current snapshot.
    pub fn top(&self) -> &[(BodyId, f64)] {
        self.pair.as_ref().map_or(&[], |p| &p.top)
    }

    /// Weight of the dust particles of the current snapshot.
    pub fn dust_weight(&self) -> f32 {
        self.pair.as_ref().map_or(1.0, |p| {
            p.fa.dust_weight + (p.fb.dust_weight - p.fa.dust_weight) * self.fraction as f32
        })
    }

    pub fn is_ready(&self) -> bool {
        self.shown.is_some()
    }

    /// True if the display is waiting for a snapshot.
    pub fn is_waiting(&self) -> bool {
        self.shown
            .is_none_or(|s| (s - self.clock.position).abs() > 1e-9)
    }

    pub fn error(&self) -> Option<String> {
        self.loader.error()
    }

    /// Current identifier of a body after the mergers already played.
    pub fn resolve(&self, id: BodyId) -> Option<BodyId> {
        let mut cur = id;
        for _ in 0..1_000 {
            match self.fates.get(&cur) {
                None => return Some(cur),
                Some(BodyFate::MergedInto(s)) => cur = *s,
                Some(BodyFate::Removed) => return None,
            }
        }
        Some(cur)
    }

    fn ensure_pair(&mut self, a: usize, b: usize) -> bool {
        if self.pair.as_ref().is_some_and(|p| p.a == a && p.b == b) {
            return true;
        }
        let (Some(fa), Some(fb)) = (self.loader.get(a), self.loader.get(b)) else {
            return false;
        };
        for f in [&fa, &fb] {
            for &(id, fate) in &f.fates {
                self.fates.insert(id, fate);
            }
        }
        let targets = if a == b {
            Vec::new()
        } else {
            build_targets(&fa, &fb)
        };
        let top = ranking::top_k(&fa.bodies.ids, &fa.bodies.masses, 10);
        self.pair = Some(Pair {
            a,
            b,
            fa,
            fb,
            targets,
            top,
        });
        true
    }

    /// Advances playback by `dt` seconds. Returns true if the displayed state
    /// changed.
    pub fn update(&mut self, dt: f64) -> bool {
        if self.frame_count() == 0 {
            return false;
        }
        let before = self.clock.position;
        self.clock.advance(dt);
        let (a, b, t) = self.clock.pair();
        self.loader.set_focus(a);

        // Read-ahead in the playback direction.
        let n = self.frame_count();
        for k in 1..=PREFETCH {
            let i = if self.clock.reverse {
                a.checked_sub(k)
            } else {
                Some(b + k)
            };
            if let Some(i) = i.filter(|&i| i < n) {
                self.loader.prefetch(i);
            }
        }

        if !self.ensure_pair(a, b) {
            // Snapshot not ready yet: the clock waits.
            if self.clock.playing {
                self.clock.position = before;
            }
            return false;
        }
        if self.shown == Some(self.clock.position) {
            return false;
        }
        let p = self.pair.as_ref().expect("paire prête");
        if p.a == p.b {
            self.state.bodies = p.fa.bodies.clone();
        } else {
            interpolate(&p.fa, &p.fb, &p.targets, t, &mut self.state.bodies);
        }
        self.state.step = p.fa.step + ((p.fb.step.saturating_sub(p.fa.step)) as f64 * t) as u64;
        self.state.time = p.fa.time + (p.fb.time - p.fa.time) * t;
        self.shown = Some(self.clock.position);
        self.fraction = t;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::RepositoryError;
    use crate::recording::RecordingHeader;
    use stellar_domain::SimulationConfig;

    fn frame(
        step: u64,
        time: f64,
        bodies: &[(BodyId, Vec3, Vec3)],
        fates: Vec<(BodyId, BodyFate)>,
    ) -> Keyframe {
        let mut b = BodySet::default();
        for &(id, p, v) in bodies {
            b.push(id, p, v, 1.0);
        }
        Keyframe {
            step,
            time,
            bodies: b,
            fates,
            dust_weight: 1.0,
            groups: Vec::new(),
        }
    }

    #[test]
    fn clock_stops_at_ends_and_steps() {
        let mut c = PlaybackClock::new(5);
        c.playing = true;
        c.speed = 10.0;
        c.advance(1.0);
        assert_eq!(c.position, 4.0);
        assert!(!c.playing);
        c.reverse = true;
        c.playing = true;
        c.advance(0.15);
        assert!((c.position - 2.5).abs() < 1e-12);
        assert_eq!(c.pair(), (2, 3, 0.5));
        c.step(1);
        assert_eq!(c.position, 3.0);
        c.step(-1);
        assert_eq!(c.position, 2.0);
        c.seek(4.0);
        assert_eq!(c.pair(), (3, 4, 1.0));
    }

    #[test]
    fn hermite_reproduces_uniform_circular_motion_closely() {
        // Circular motion of period 2π, snapshots 0.5 apart.
        let pos = |t: f64| Vec3::new(t.cos(), t.sin(), 0.0);
        let vel = |t: f64| Vec3::new(-t.sin(), t.cos(), 0.0);
        let a = frame(0, 0.0, &[(1, pos(0.0), vel(0.0))], vec![]);
        let b = frame(10, 0.5, &[(1, pos(0.5), vel(0.5))], vec![]);
        let targets = build_targets(&a, &b);
        let mut out = BodySet::default();
        for k in 0..=10 {
            let t = k as f64 / 10.0;
            interpolate(&a, &b, &targets, t, &mut out);
            let err = (out.positions[0] - pos(0.5 * t)).length();
            assert!(err < 1e-3, "erreur {err} à t = {t}");
        }
        // Exact ends.
        interpolate(&a, &b, &targets, 1.0, &mut out);
        assert!((out.positions[0] - pos(0.5)).length() < 1e-12);
    }

    #[test]
    fn absorbed_bodies_join_their_survivor() {
        let a = frame(
            0,
            0.0,
            &[
                (1, Vec3::ZERO, Vec3::ZERO),
                (2, Vec3::X, Vec3::ZERO),
                (3, Vec3::Y, Vec3::ZERO),
                (4, Vec3::Z, Vec3::ZERO),
            ],
            vec![],
        );
        // 3 → 2 → 1 (chain), 4 removed.
        let b = frame(
            1,
            1.0,
            &[(1, Vec3::new(5.0, 0.0, 0.0), Vec3::ZERO)],
            vec![
                (3, BodyFate::MergedInto(2)),
                (2, BodyFate::MergedInto(1)),
                (4, BodyFate::Removed),
            ],
        );
        let targets = build_targets(&a, &b);
        assert_eq!(
            targets,
            vec![
                Target::Same(0),
                Target::Merged(0),
                Target::Merged(0),
                Target::Gone
            ]
        );
        let mut out = BodySet::default();
        interpolate(&a, &b, &targets, 1.0, &mut out);
        assert!((out.positions[2] - Vec3::new(5.0, 0.0, 0.0)).length() < 1e-12);
    }

    struct MemReader {
        info: RecordingInfo,
        frames: Vec<Keyframe>,
    }

    impl RecordingReader for MemReader {
        fn info(&self) -> &RecordingInfo {
            &self.info
        }
        fn read_frame(&mut self, index: usize) -> Result<Keyframe, RepositoryError> {
            self.frames
                .get(index)
                .cloned()
                .ok_or_else(|| RepositoryError::Format("hors limites".into()))
        }
    }

    #[test]
    fn player_waits_for_frames_then_interpolates() {
        let frames: Vec<Keyframe> = (0..6)
            .map(|k| {
                frame(
                    k,
                    k as f64,
                    &[(1, Vec3::new(k as f64, 0.0, 0.0), Vec3::X)],
                    vec![],
                )
            })
            .collect();
        let info = RecordingInfo {
            header: RecordingHeader {
                config: SimulationConfig::default(),
                interval: 1,
                start_step: 0,
                created_at_unix: 0,
                dust_budget: None,
            },
            frames: frames
                .iter()
                .map(|f| crate::recording::FrameMeta {
                    step: f.step,
                    time: f.time,
                    count: 1,
                })
                .collect(),
        };
        let mut p = ReplayPlayer::new(Box::new(MemReader { info, frames }));
        let mut ready = false;
        for _ in 0..200 {
            if p.update(0.0) {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready);
        p.clock.seek(2.5);
        for _ in 0..200 {
            if p.update(0.0) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!((p.state().bodies.positions[0].x - 2.5).abs() < 1e-9);
        assert!((p.state().time - 2.5).abs() < 1e-9);
    }
}
