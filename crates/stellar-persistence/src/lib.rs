//! # stellar-persistence
//!
//! Implémentation de [`SimulationRepository`] sur le système de fichiers.
//!
//! ## Format `.stellar` (version 3)
//!
//! | Octets      | Contenu                                         |
//! |-------------|-------------------------------------------------|
//! | 8           | signature `STELLAR\0`                           |
//! | 4 (LE)      | version du format                               |
//! | 4 (LE)      | taille L de l'en-tête                           |
//! | L           | en-tête : [`SaveSummary`] encodé en bincode     |
//! | reste       | [`SaveData`] encodé en bincode, compressé zstd  |
//!
//! L'en-tête permet de lister les sauvegardes sans tout décompresser.

pub mod recording;

pub use recording::FileRecordingRepository;

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use stellar_application::{RepositoryError, SaveData, SaveSummary, SimulationRepository};

pub const EXTENSION: &str = "stellar";
const MAGIC: &[u8; 8] = b"STELLAR\0";
/// Version 2 : lien vers un enregistrement suspendu, masse initiale.
/// Version 3 : bascule de la caméra (`ViewState::roll`).
const VERSION: u32 = 3;
const MAX_HEADER: u32 = 1 << 20;
const ZSTD_LEVEL: i32 = 3;

/// Dépôt de sauvegardes rangées dans un dossier.
#[derive(Debug, Clone)]
pub struct FileRepository {
    dir: PathBuf,
}

impl FileRepository {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn directory(&self) -> &Path {
        &self.dir
    }

    fn unique_path(&self, name: &str) -> PathBuf {
        let stem = sanitize(name);
        let mut path = self.dir.join(format!("{stem}.{EXTENSION}"));
        let mut k = 2;
        while path.exists() {
            path = self.dir.join(format!("{stem}_{k}.{EXTENSION}"));
            k += 1;
        }
        path
    }
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_owned();
    if s.is_empty() {
        "simulation".to_owned()
    } else {
        s
    }
}

fn io(e: std::io::Error) -> RepositoryError {
    RepositoryError::Io(e.to_string())
}

fn fmt(e: impl std::fmt::Display) -> RepositoryError {
    RepositoryError::Format(e.to_string())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Point de vue des versions 1 et 2, sans bascule.
mod legacy {
    use serde::{Deserialize, Serialize};
    use stellar_application::ViewState;
    use stellar_domain::BodyId;

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    pub struct View {
        pub position: [f64; 3],
        pub yaw: f64,
        pub pitch: f64,
        pub fov_y: f64,
        pub followed: Option<BodyId>,
        pub orbit_distance: f64,
    }

    impl From<View> for ViewState {
        fn from(v: View) -> Self {
            ViewState {
                position: v.position,
                yaw: v.yaw,
                pitch: v.pitch,
                roll: 0.0,
                fov_y: v.fov_y,
                followed: v.followed,
                orbit_distance: v.orbit_distance,
            }
        }
    }
}

/// Formes de la version 2.
mod v2 {
    use serde::Deserialize;
    use stellar_application::{RecordingLink, SaveData};
    use stellar_domain::{SimulationConfig, SimulationState};

    #[derive(Deserialize)]
    pub struct Data {
        config: SimulationConfig,
        state: SimulationState,
        view: Option<super::legacy::View>,
        recording: Option<RecordingLink>,
    }

    impl From<Data> for SaveData {
        fn from(d: Data) -> Self {
            SaveData {
                config: d.config,
                state: d.state,
                view: d.view.map(Into::into),
                recording: d.recording,
            }
        }
    }
}

/// Formes de la version 1, pour relire les anciennes sauvegardes.
mod v1 {
    use serde::Deserialize;
    use stellar_application::{SaveData, SaveSummary};
    use stellar_domain::{BodySet, SimulationConfig, SimulationState};

    #[derive(Deserialize)]
    pub struct Summary {
        name: String,
        saved_at_unix: i64,
        body_count: usize,
        step: u64,
        time: f64,
        engine: String,
    }

    impl From<Summary> for SaveSummary {
        fn from(s: Summary) -> Self {
            SaveSummary {
                path: Default::default(),
                name: s.name,
                saved_at_unix: s.saved_at_unix,
                body_count: s.body_count,
                step: s.step,
                time: s.time,
                engine: s.engine,
                recording: None,
            }
        }
    }

    #[derive(Deserialize)]
    struct State {
        bodies: BodySet,
        step: u64,
        time: f64,
        initial_energy: Option<f64>,
    }

    #[derive(Deserialize)]
    pub struct Data {
        config: SimulationConfig,
        state: State,
        view: Option<super::legacy::View>,
    }

    impl From<Data> for SaveData {
        fn from(d: Data) -> Self {
            SaveData {
                config: d.config,
                state: SimulationState {
                    bodies: d.state.bodies,
                    step: d.state.step,
                    time: d.state.time,
                    initial_energy: d.state.initial_energy,
                    initial_mass: None,
                },
                view: d.view.map(Into::into),
                recording: None,
            }
        }
    }
}

fn read_header(r: &mut impl Read) -> Result<(u32, SaveSummary), RepositoryError> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)
        .map_err(|_| fmt("fichier tronqué"))?;
    if &magic != MAGIC {
        return Err(fmt(
            "signature absente : ce n'est pas une sauvegarde Stellar",
        ));
    }
    let mut word = [0u8; 4];
    r.read_exact(&mut word).map_err(io)?;
    let version = u32::from_le_bytes(word);
    if !(1..=VERSION).contains(&version) {
        return Err(RepositoryError::UnsupportedVersion(version));
    }
    r.read_exact(&mut word).map_err(io)?;
    let len = u32::from_le_bytes(word);
    if len > MAX_HEADER {
        return Err(fmt("en-tête démesuré"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(io)?;
    let summary = if version == 1 {
        bincode::deserialize::<v1::Summary>(&buf)
            .map_err(fmt)?
            .into()
    } else {
        bincode::deserialize(&buf).map_err(fmt)?
    };
    Ok((version, summary))
}

impl SimulationRepository for FileRepository {
    fn save(&self, name: &str, data: &SaveData) -> Result<PathBuf, RepositoryError> {
        fs::create_dir_all(&self.dir).map_err(io)?;
        let path = self.unique_path(name);
        let summary = SaveSummary {
            path: PathBuf::new(),
            name: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            saved_at_unix: now_unix(),
            body_count: data.state.bodies.len(),
            step: data.state.step,
            time: data.state.time,
            engine: data.config.engine.clone(),
            recording: data
                .recording
                .as_ref()
                .and_then(|l| l.path.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
        };
        let header = bincode::serialize(&summary).map_err(fmt)?;

        // Écriture dans un fichier temporaire puis renommage : jamais de
        // sauvegarde à moitié écrite.
        let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
        {
            let mut w = BufWriter::new(File::create(&tmp).map_err(io)?);
            w.write_all(MAGIC).map_err(io)?;
            w.write_all(&VERSION.to_le_bytes()).map_err(io)?;
            w.write_all(&(header.len() as u32).to_le_bytes())
                .map_err(io)?;
            w.write_all(&header).map_err(io)?;
            let mut enc = zstd::stream::Encoder::new(w, ZSTD_LEVEL).map_err(io)?;
            bincode::serialize_into(&mut enc, data).map_err(fmt)?;
            let mut w = enc.finish().map_err(io)?;
            w.flush().map_err(io)?;
        }
        fs::rename(&tmp, &path).map_err(io)?;
        Ok(path)
    }

    fn load(&self, path: &Path) -> Result<SaveData, RepositoryError> {
        let mut r = BufReader::new(File::open(path).map_err(io)?);
        let (version, _) = read_header(&mut r)?;
        let dec = zstd::stream::Decoder::with_buffer(r).map_err(io)?;
        let data: SaveData = if version == 1 {
            bincode::deserialize_from::<_, v1::Data>(dec)
                .map_err(fmt)?
                .into()
        } else if version == 2 {
            bincode::deserialize_from::<_, v2::Data>(dec)
                .map_err(fmt)?
                .into()
        } else {
            bincode::deserialize_from(dec).map_err(fmt)?
        };
        if !data.state.bodies.is_consistent() {
            return Err(fmt("tableaux de corps incohérents"));
        }
        Ok(data)
    }

    fn list(&self) -> Result<Vec<SaveSummary>, RepositoryError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
                continue;
            }
            let Ok(file) = File::open(&path) else {
                continue;
            };
            // Les fichiers illisibles sont ignorés plutôt que de bloquer la liste.
            if let Ok((_, mut s)) = read_header(&mut BufReader::new(file)) {
                s.path = path;
                out.push(s);
            }
        }
        out.sort_by(|a, b| {
            b.saved_at_unix
                .cmp(&a.saved_at_unix)
                .then(a.name.cmp(&b.name))
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_application::ViewState;
    use stellar_domain::{BodySet, SimulationConfig, SimulationState, Vec3};

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("stellar-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn sample() -> SaveData {
        let mut b = BodySet::default();
        for i in 0..1_000u64 {
            let f = i as f64;
            b.push(
                i,
                Vec3::new(f, -f, 0.5 * f),
                Vec3::new(0.1, 0.2, f),
                1.0 + f,
            );
        }
        SaveData {
            config: SimulationConfig::default(),
            state: SimulationState {
                bodies: b,
                step: 42,
                time: 1.5,
                initial_energy: Some(-3.0),
                initial_mass: Some(1_000.0),
            },
            view: Some(ViewState {
                position: [1.0, 2.0, 3.0],
                yaw: 0.5,
                pitch: -0.2,
                roll: 0.3,
                fov_y: 1.0,
                followed: Some(7),
                orbit_distance: 12.0,
            }),
            recording: Some(stellar_application::RecordingLink {
                path: PathBuf::from("recordings/rec_test.stellarec"),
                interval: 10,
                dust_budget: Some(500),
                last_frame_step: 40,
                next_step: 50,
                pending: vec![(3, stellar_application::BodyFate::MergedInto(1))],
            }),
        }
    }

    #[test]
    fn reads_version_1_files() {
        #[derive(serde::Serialize)]
        struct SummaryV1<'a> {
            name: &'a str,
            saved_at_unix: i64,
            body_count: usize,
            step: u64,
            time: f64,
            engine: &'a str,
        }
        #[derive(serde::Serialize)]
        struct StateV1<'a> {
            bodies: &'a BodySet,
            step: u64,
            time: f64,
            initial_energy: Option<f64>,
        }
        #[derive(serde::Serialize)]
        struct DataV1<'a> {
            config: &'a SimulationConfig,
            state: StateV1<'a>,
            view: Option<legacy::View>,
        }
        let dir = temp_dir("v1");
        fs::create_dir_all(&dir).unwrap();
        let reference = sample();
        let header = bincode::serialize(&SummaryV1 {
            name: "ancienne",
            saved_at_unix: 7,
            body_count: reference.state.bodies.len(),
            step: 42,
            time: 1.5,
            engine: "cpu-barnes-hut",
        })
        .unwrap();
        let body = bincode::serialize(&DataV1 {
            config: &reference.config,
            state: StateV1 {
                bodies: &reference.state.bodies,
                step: 42,
                time: 1.5,
                initial_energy: Some(-3.0),
            },
            view: reference.view.map(|v| legacy::View {
                position: v.position,
                yaw: v.yaw,
                pitch: v.pitch,
                fov_y: v.fov_y,
                followed: v.followed,
                orbit_distance: v.orbit_distance,
            }),
        })
        .unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&zstd::bulk::compress(&body, 3).unwrap());
        let path = dir.join("ancienne.stellar");
        fs::write(&path, bytes).unwrap();

        let repo = FileRepository::new(&dir);
        let list = repo.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].recording, None);
        let data = repo.load(&path).unwrap();
        assert_eq!(data.state.bodies, reference.state.bodies);
        assert_eq!(data.state.initial_mass, None);
        assert_eq!(data.recording, None);
        let view = data.view.unwrap();
        assert_eq!(view.roll, 0.0);
        assert_eq!(view.yaw, 0.5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_version_2_files() {
        #[derive(serde::Serialize)]
        struct DataV2<'a> {
            config: &'a SimulationConfig,
            state: &'a SimulationState,
            view: Option<legacy::View>,
            recording: &'a Option<stellar_application::RecordingLink>,
        }
        let dir = temp_dir("v2");
        fs::create_dir_all(&dir).unwrap();
        let reference = sample();
        let mut summary = repo_summary(&reference);
        summary.name = "v2".into();
        let header = bincode::serialize(&summary).unwrap();
        let v = reference.view.unwrap();
        let body = bincode::serialize(&DataV2 {
            config: &reference.config,
            state: &reference.state,
            view: Some(legacy::View {
                position: v.position,
                yaw: v.yaw,
                pitch: v.pitch,
                fov_y: v.fov_y,
                followed: v.followed,
                orbit_distance: v.orbit_distance,
            }),
            recording: &reference.recording,
        })
        .unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&zstd::bulk::compress(&body, 3).unwrap());
        let path = dir.join("v2.stellar");
        fs::write(&path, bytes).unwrap();

        let repo = FileRepository::new(&dir);
        assert_eq!(repo.list().unwrap().len(), 1);
        let data = repo.load(&path).unwrap();
        assert_eq!(data.state, reference.state);
        assert_eq!(data.recording, reference.recording);
        let view = data.view.unwrap();
        assert_eq!(view.roll, 0.0);
        assert_eq!(view.orbit_distance, 12.0);
        let _ = fs::remove_dir_all(&dir);
    }

    fn repo_summary(d: &SaveData) -> SaveSummary {
        SaveSummary {
            path: PathBuf::new(),
            name: String::new(),
            saved_at_unix: 1,
            body_count: d.state.bodies.len(),
            step: d.state.step,
            time: d.state.time,
            engine: d.config.engine.clone(),
            recording: None,
        }
    }

    #[test]
    fn roundtrip_and_listing() {
        let dir = temp_dir("roundtrip");
        let repo = FileRepository::new(&dir);
        assert!(repo.list().unwrap().is_empty());

        let data = sample();
        let p1 = repo.save("ma simulation", &data).unwrap();
        let p2 = repo.save("ma simulation", &data).unwrap();
        assert_ne!(p1, p2, "les noms doivent être uniques");
        assert_eq!(repo.load(&p1).unwrap(), data);

        let list = repo.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].body_count, 1_000);
        assert_eq!(list[0].step, 42);
        assert_eq!(list[0].recording.as_deref(), Some("rec_test.stellarec"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_foreign_files() {
        let dir = temp_dir("foreign");
        fs::create_dir_all(&dir).unwrap();
        let bogus = dir.join("bogus.stellar");
        fs::write(&bogus, b"not a save file at all").unwrap();
        let repo = FileRepository::new(&dir);
        assert!(repo.load(&bogus).is_err());
        assert!(repo.list().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
