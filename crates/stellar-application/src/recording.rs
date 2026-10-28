//! Enregistrement d'une simulation pour relecture fluide.
//!
//! Le thread de simulation produit un **instantané** ([`Keyframe`]) tous les
//! N pas : positions, vitesses, masses et devenir des corps disparus depuis
//! l'instantané précédent. Un thread d'écriture dédié ([`Recorder`]) les trie,
//! les compresse et les écrit via le port [`RecordingWriter`], sans ralentir
//! le calcul au-delà d'une file de quelques instantanés.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};
use stellar_domain::{BodyId, BodySet, SimulationConfig};

use crate::ports::RepositoryError;

/// Devenir d'un corps disparu entre deux instantanés.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyFate {
    MergedInto(BodyId),
    Removed,
}

/// Un instantané enregistré. Les corps sont triés par identifiant croissant.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keyframe {
    pub step: u64,
    pub time: f64,
    pub bodies: BodySet,
    /// Corps disparus depuis l'instantané précédent.
    pub fates: Vec<(BodyId, BodyFate)>,
    /// Nombre de particules de poussière représentées par chaque particule de
    /// poussière enregistrée (1 sans sous-échantillonnage).
    pub dust_weight: f32,
    /// Sous-système de chaque corps (vide : un seul) ; chaque sous-système a
    /// sa propre boîte de quantification, pour garder la précision d'un amas
    /// parti très loin.
    pub groups: Vec<u16>,
}

/// Vrai pour une particule de poussière (masse initiale 1, jamais fusionnée).
#[inline]
pub fn is_dust(mass: f64) -> bool {
    mass <= 1.0 + 1e-9
}

/// Mélange de bits (splitmix64) : sélection pseudo-aléatoire mais stable d'un
/// corps d'après son identifiant.
#[inline]
fn id_hash(id: BodyId) -> u64 {
    let mut z = id.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Budget de poussière : tous les corps massifs sont gardés, et au plus
/// `budget` particules de poussière, choisies d'après leur identifiant. Les
/// sélections sont emboîtées : quand la fraction gardée augmente, des
/// particules s'ajoutent sans qu'aucune ne disparaisse.
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
    /// Trie les corps par identifiant (ordre requis par le format et la relecture).
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

/// En-tête d'un enregistrement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingHeader {
    pub config: SimulationConfig,
    /// Nombre de pas entre deux instantanés.
    pub interval: u32,
    pub start_step: u64,
    pub created_at_unix: i64,
    /// Nombre maximal de particules de poussière par instantané (`None` = toutes).
    pub dust_budget: Option<u64>,
}

/// Lien entre une sauvegarde et un enregistrement suspendu : tout ce qu'il
/// faut pour le poursuivre exactement là où il s'était arrêté.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingLink {
    pub path: PathBuf,
    pub interval: u32,
    pub dust_budget: Option<u64>,
    /// Pas du dernier instantané écrit avant la suspension : les instantanés
    /// postérieurs éventuels sont supprimés à la reprise.
    pub last_frame_step: u64,
    /// Pas du prochain instantané à écrire.
    pub next_step: u64,
    /// Devenirs survenus depuis le dernier instantané.
    pub pending: Vec<(BodyId, BodyFate)>,
}

/// Métadonnées d'un instantané (sans les corps).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameMeta {
    pub step: u64,
    pub time: f64,
    pub count: u32,
}

/// Description complète d'un enregistrement ouvert.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingInfo {
    pub header: RecordingHeader,
    pub frames: Vec<FrameMeta>,
}

/// Résumé pour la liste de l'écran d'accueil.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub name: String,
    pub created_at_unix: i64,
    pub frame_count: usize,
    pub first_count: u32,
    pub duration: f64,
    pub bytes: u64,
    /// Faux si l'enregistrement n'a pas été clos proprement (index reconstruit).
    pub complete: bool,
}

/// Port d'écriture d'un enregistrement.
pub trait RecordingWriter: Send {
    /// Fichier écrit.
    fn path(&self) -> PathBuf;
    fn write_frame(&mut self, frame: &Keyframe) -> Result<u64, RepositoryError>;
    /// Clôt le fichier (écriture de l'index) et renvoie son chemin.
    fn finish(self: Box<Self>) -> Result<PathBuf, RepositoryError>;
}

/// Port de lecture d'un enregistrement (accès direct à chaque instantané).
pub trait RecordingReader: Send {
    fn info(&self) -> &RecordingInfo;
    fn read_frame(&mut self, index: usize) -> Result<Keyframe, RepositoryError>;
}

/// Port de stockage des enregistrements.
pub trait RecordingRepository: Send + Sync {
    fn create(
        &self,
        name: &str,
        header: &RecordingHeader,
    ) -> Result<Box<dyn RecordingWriter>, RepositoryError>;
    fn open(&self, path: &Path) -> Result<Box<dyn RecordingReader>, RepositoryError>;
    fn list(&self) -> Result<Vec<RecordingSummary>, RepositoryError>;
    /// Rouvre un enregistrement pour le poursuivre : les instantanés
    /// postérieurs à `keep_through_step` sont supprimés, l'écriture reprend à
    /// la suite.
    fn resume(
        &self,
        path: &Path,
        keep_through_step: u64,
    ) -> Result<Box<dyn RecordingWriter>, RepositoryError>;
}

/// État d'un enregistrement en cours, consultable par l'interface.
#[derive(Debug, Default)]
pub struct RecordingStatus {
    pub active: AtomicBool,
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    pub interval: AtomicU64,
    pub error: Mutex<Option<String>>,
    pub last_path: Mutex<Option<PathBuf>>,
    /// Fichier de l'enregistrement en cours.
    pub current_path: Mutex<Option<PathBuf>>,
}

enum Msg {
    Frame(Keyframe),
}

/// Thread d'écriture : reçoit les instantanés et les confie au port.
pub struct Recorder {
    tx: Option<SyncSender<Msg>>,
    thread: Option<JoinHandle<Result<PathBuf, RepositoryError>>>,
    status: Arc<RecordingStatus>,
}

/// Instantanés en attente d'écriture avant que le calcul ne patiente.
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

    /// Nombre d'instantanés transmis depuis le démarrage.
    pub fn status(&self) -> &RecordingStatus {
        &self.status
    }

    /// Transmet un instantané (bloque si la file est pleine).
    pub fn push(&self, frame: Keyframe) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Frame(frame));
        }
    }

    /// Termine l'écriture et renvoie le chemin du fichier.
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
        // Tous les corps massifs sont conservés.
        assert_eq!(
            small.bodies.masses.iter().filter(|&&m| m > 1.0).count(),
            100
        );
        // Budget respecté à quelques pour cent près.
        let dust = small.bodies.len() - 100;
        assert!((9_000..11_000).contains(&dust), "{dust}");
        assert!((small.dust_weight - 9.99).abs() < 0.1);
        // Emboîtement : la petite sélection est incluse dans la grande.
        let big: std::collections::HashSet<_> = large.bodies.ids.iter().collect();
        assert!(small.bodies.ids.iter().all(|id| big.contains(id)));
        // Budget non atteint : rien n'est exclu.
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
