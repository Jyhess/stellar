//! Recording of a simulation for smooth playback.
//!
//! The simulation thread produces a **snapshot** ([`Keyframe`]) every N
//! steps: positions, velocities, masses and fate of the bodies that
//! disappeared since the previous snapshot. A dedicated writer thread
//! ([`Recorder`]) sorts, compresses and writes them through the
//! [`RecordingWriter`] port, without slowing the computation down beyond a
//! queue of a few snapshots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};
use stellar_domain::{BodyId, BodySet, SimulationConfig};

use crate::ports::RepositoryError;

/// Fate of a body that disappeared between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyFate {
    MergedInto(BodyId),
    Removed,
}

/// A recorded snapshot. Bodies are sorted by increasing identifier.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keyframe {
    pub step: u64,
    pub time: f64,
    pub bodies: BodySet,
    /// Bodies that disappeared since the previous snapshot.
    pub fates: Vec<(BodyId, BodyFate)>,
    /// Number of dust particles represented by each recorded dust particle (1
    /// without subsampling).
    pub dust_weight: f32,
    /// Subsystem of each body (empty: a single one); each subsystem has its own
    /// quantization box, to keep the precision of a cluster gone very far
    /// away.
    pub groups: Vec<u16>,
}

/// True for a dust particle (initial mass 1, never merged).
#[inline]
pub fn is_dust(mass: f64) -> bool {
    mass <= 1.0 + 1e-9
}

/// Bit mixing (splitmix64): pseudo-random but stable selection of a body
/// from its identifier.
#[inline]
fn id_hash(id: BodyId) -> u64 {
    let mut z = id.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Dust budget: every massive body is kept, plus at most `budget` dust
/// particles, chosen from their identifier. Selections are nested: when the
/// kept fraction grows, particles are added and none disappears.
pub fn apply_dust_budget(frame: &mut Keyframe, budget: u64) {
    let dust = frame.bodies.masses.iter().filter(|&&m| is_dust(m)).count() as u64;
    if dust <= budget {
        frame.dust_weight = 1.0;
        return;
    }
    let fraction = budget as f64 / dust as f64;
    let threshold = (fraction * u64::MAX as f64) as u64;
    let b = &frame.bodies;
    let keep: Vec<bool> = (0..b.len())
        .map(|i| !is_dust(b.masses[i]) || id_hash(b.ids[i]) <= threshold)
        .collect();
    let mut out = BodySet::with_capacity(budget as usize + 1024);
    let mut groups = Vec::new();
    for (i, &k) in keep.iter().enumerate() {
        if k {
            out.push(b.ids[i], b.positions[i], b.velocities[i], b.masses[i]);
            if let Some(&g) = frame.groups.get(i) {
                groups.push(g);
            }
        }
    }
    frame.bodies = out;
    frame.groups = groups;
    frame.dust_weight = (1.0 / fraction) as f32;
}

impl Keyframe {
    /// Sorts the bodies by identifier (order required by the format and playback).
    pub fn sort_by_id(&mut self) {
        let b = &self.bodies;
        if b.ids.windows(2).all(|w| w[0] < w[1]) {
            return;
        }
        let mut order: Vec<u32> = (0..b.len() as u32).collect();
        order.sort_unstable_by_key(|&i| b.ids[i as usize]);
        let pick = |i: &u32| *i as usize;
        let sorted = BodySet {
            ids: order.iter().map(|i| b.ids[pick(i)]).collect(),
            positions: order.iter().map(|i| b.positions[pick(i)]).collect(),
            velocities: order.iter().map(|i| b.velocities[pick(i)]).collect(),
            masses: order.iter().map(|i| b.masses[pick(i)]).collect(),
        };
        if !self.groups.is_empty() {
            self.groups = order.iter().map(|i| self.groups[pick(i)]).collect();
        }
        self.bodies = sorted;
    }
}

/// Header of a recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingHeader {
    pub config: SimulationConfig,
    /// Number of steps between two snapshots.
    pub interval: u32,
    pub start_step: u64,
    pub created_at_unix: i64,
    /// Maximum number of dust particles per snapshot (`None` = all).
    pub dust_budget: Option<u64>,
}

/// Link between a save and a suspended recording: everything needed to
/// continue it exactly where it stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingLink {
    pub path: PathBuf,
    pub interval: u32,
    pub dust_budget: Option<u64>,
    /// Step of the last snapshot written before the suspension: any later
    /// snapshots are deleted on resume.
    pub last_frame_step: u64,
    /// Step of the next snapshot to write.
    pub next_step: u64,
    /// Fates that happened since the last snapshot.
    pub pending: Vec<(BodyId, BodyFate)>,
}

/// Metadata of a snapshot (without the bodies).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameMeta {
    pub step: u64,
    pub time: f64,
    pub count: u32,
}

/// Full description of an open recording.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingInfo {
    pub header: RecordingHeader,
    pub frames: Vec<FrameMeta>,
}

/// Summary for the list of the home screen.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub name: String,
    pub created_at_unix: i64,
    pub frame_count: usize,
    pub first_count: u32,
    pub duration: f64,
    pub bytes: u64,
    /// False if the recording was not closed cleanly (index rebuilt).
    pub complete: bool,
}

/// Writing port of a recording.
pub trait RecordingWriter: Send {
    /// File being written.
    fn path(&self) -> PathBuf;
    fn write_frame(&mut self, frame: &Keyframe) -> Result<u64, RepositoryError>;
    /// Closes the file (writing the index) and returns its path.
    fn finish(self: Box<Self>) -> Result<PathBuf, RepositoryError>;
}

/// Reading port of a recording (direct access to each snapshot).
pub trait RecordingReader: Send {
    fn info(&self) -> &RecordingInfo;
    fn read_frame(&mut self, index: usize) -> Result<Keyframe, RepositoryError>;
}

/// Storage port of recordings.
pub trait RecordingRepository: Send + Sync {
    fn create(
        &self,
        name: &str,
        header: &RecordingHeader,
    ) -> Result<Box<dyn RecordingWriter>, RepositoryError>;
    fn open(&self, path: &Path) -> Result<Box<dyn RecordingReader>, RepositoryError>;
    fn list(&self) -> Result<Vec<RecordingSummary>, RepositoryError>;
    /// Reopens a recording to continue it: snapshots after `keep_through_step`
    /// are deleted, and writing resumes after the last one kept.
    fn resume(
        &self,
        path: &Path,
        keep_through_step: u64,
    ) -> Result<Box<dyn RecordingWriter>, RepositoryError>;

    /// Deletes a recording (to the recycle bin if the repository provides it).
    fn delete(&self, path: &Path) -> Result<(), RepositoryError>;

    /// Renames a recording (same folder); returns the new path.
    fn rename(&self, path: &Path, name: &str) -> Result<PathBuf, RepositoryError>;
}

/// State of a running recording, readable by the interface.
#[derive(Debug, Default)]
pub struct RecordingStatus {
    pub active: AtomicBool,
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    pub interval: AtomicU64,
    pub error: Mutex<Option<String>>,
    pub last_path: Mutex<Option<PathBuf>>,
    /// File of the running recording.
    pub current_path: Mutex<Option<PathBuf>>,
}

enum Msg {
    Frame(Keyframe),
}

/// Writer thread: receives the snapshots and hands them to the port.
pub struct Recorder {
    tx: Option<SyncSender<Msg>>,
    thread: Option<JoinHandle<Result<PathBuf, RepositoryError>>>,
    status: Arc<RecordingStatus>,
}

/// Snapshots waiting to be written before the computation has to wait.
const QUEUE: usize = 3;

impl Recorder {
    pub fn start(
        mut writer: Box<dyn RecordingWriter>,
        status: Arc<RecordingStatus>,
        dust_budget: Option<u64>,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE);
        *status.current_path.lock().expect("verrou") = Some(writer.path());
        let st = Arc::clone(&status);
        let thread = std::thread::Builder::new()
            .name("stellar-recorder".into())
            .spawn(move || {
                for msg in rx {
                    let Msg::Frame(mut frame) = msg;
                    if let Some(budget) = dust_budget {
                        apply_dust_budget(&mut frame, budget);
                    }
                    frame.sort_by_id();
                    match writer.write_frame(&frame) {
                        Ok(bytes) => {
                            st.frames.fetch_add(1, Ordering::Relaxed);
                            st.bytes.fetch_add(bytes, Ordering::Relaxed);
                        }
                        Err(e) => {
                            *st.error.lock().expect("verrou") = Some(e.to_string());
                            return Err(e);
                        }
                    }
                }
                writer.finish()
            })
            .expect("impossible de lancer le thread d'enregistrement");
        status.active.store(true, Ordering::Release);
        Self {
            tx: Some(tx),
            thread: Some(thread),
            status,
        }
    }

    /// State of the recording (snapshots written, bytes, errors…).
    pub fn status(&self) -> &RecordingStatus {
        &self.status
    }

    /// Hands over a snapshot (blocks if the queue is full).
    pub fn push(&self, frame: Keyframe) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Frame(frame));
        }
    }

    /// Finishes writing and returns the file's path.
    pub fn finish(mut self) -> Result<PathBuf, RepositoryError> {
        self.close()
    }

    fn close(&mut self) -> Result<PathBuf, RepositoryError> {
        self.tx.take();
        let result = match self.thread.take() {
            Some(t) => t.join().unwrap_or_else(|_| {
                Err(RepositoryError::Io(
                    "thread d'enregistrement interrompu".into(),
                ))
            }),
            None => Err(RepositoryError::Io("enregistrement déjà clos".into())),
        };
        self.status.active.store(false, Ordering::Release);
        *self.status.current_path.lock().expect("verrou") = None;
        match &result {
            Ok(p) => *self.status.last_path.lock().expect("verrou") = Some(p.clone()),
            Err(e) => *self.status.error.lock().expect("verrou") = Some(e.to_string()),
        }
        result
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_domain::Vec3;

    #[test]
    fn dust_budget_keeps_heavy_bodies_and_nests_selections() {
        let mut base = Keyframe {
            dust_weight: 1.0,
            ..Default::default()
        };
        for id in 0..100_000u64 {
            let m = if id % 1000 == 0 { 5.0 } else { 1.0 };
            base.bodies.push(id, Vec3::ZERO, Vec3::ZERO, m);
        }
        let mut small = base.clone();
        apply_dust_budget(&mut small, 10_000);
        let mut large = base.clone();
        apply_dust_budget(&mut large, 40_000);
        // Every massive body is kept.
        assert_eq!(
            small.bodies.masses.iter().filter(|&&m| m > 1.0).count(),
            100
        );
        // Budget respected to within a few percent.
        let dust = small.bodies.len() - 100;
        assert!((9_000..11_000).contains(&dust), "{dust}");
        assert!((small.dust_weight - 9.99).abs() < 0.1);
        // Nesting: the small selection is included in the large one.
        let big: std::collections::HashSet<_> = large.bodies.ids.iter().collect();
        assert!(small.bodies.ids.iter().all(|id| big.contains(id)));
        // Budget not reached: nothing is excluded.
        let mut all = base.clone();
        apply_dust_budget(&mut all, 1_000_000);
        assert_eq!(all.bodies.len(), 100_000);
        assert_eq!(all.dust_weight, 1.0);
    }

    #[test]
    fn sort_by_id_keeps_rows_together() {
        let mut k = Keyframe::default();
        k.bodies.push(5, Vec3::X, Vec3::Y, 2.0);
        k.bodies.push(1, Vec3::Z, Vec3::X, 3.0);
        k.sort_by_id();
        assert_eq!(k.bodies.ids, vec![1, 5]);
        assert_eq!(k.bodies.positions, vec![Vec3::Z, Vec3::X]);
        assert_eq!(k.bodies.masses, vec![3.0, 2.0]);
    }
}
