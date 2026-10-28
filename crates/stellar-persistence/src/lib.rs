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
//!
//! bincode n'étant pas auto-descriptif, tout changement de [`SaveData`] ou
//! de [`SaveSummary`] impose une nouvelle version ; les fichiers d'une autre
//! version sont refusés.

pub mod recording;

pub use recording::FileRecordingRepository;

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use stellar_application::{RepositoryError, SaveData, SaveSummary, SimulationRepository};

pub const EXTENSION: &str = "stellar";
const MAGIC: &[u8; 8] = b"STELLAR\0";
/// Version du format : seule celle-ci est lue.
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

fn read_header(r: &mut impl Read) -> Result<SaveSummary, RepositoryError> {
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
    if version != VERSION {
        return Err(RepositoryError::UnsupportedVersion(version));
    }
    r.read_exact(&mut word).map_err(io)?;
    let len = u32::from_le_bytes(word);
    if len > MAX_HEADER {
        return Err(fmt("en-tête démesuré"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(io)?;
    bincode::deserialize(&buf).map_err(fmt)
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
                .map(|l| l.path.as_path())
                .or(data.continue_recording.as_deref())
                .and_then(|p| p.file_name())
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
        read_header(&mut r)?;
        let dec = zstd::stream::Decoder::with_buffer(r).map_err(io)?;
        let data: SaveData = bincode::deserialize_from(dec).map_err(fmt)?;
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
            if let Ok(mut s) = read_header(&mut BufReader::new(file)) {
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
                barycenter: false,
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
            continue_recording: None,
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
    fn rejects_other_versions() {
        let dir = temp_dir("version");
        let repo = FileRepository::new(&dir);
        let path = repo.save("autre", &sample()).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes[8..12].copy_from_slice(&(VERSION - 1).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            repo.load(&path),
            Err(RepositoryError::UnsupportedVersion(v)) if v == VERSION - 1
        ));
        assert!(repo.list().unwrap().is_empty());
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
