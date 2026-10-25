//! Format d'enregistrement `.stellarec` (version 1).
//!
//! ```text
//! signature « STELREC\0 » · version (u32) · taille L (u32) · en-tête bincode (L octets)
//! instantané* :
//!     « FRAM » (u32) · pas (u64) · temps (f64) · nb de corps (u32)
//!     taille C (u32) · charge utile compressée zstd (C octets)
//! index (en fin de fichier, écrit à la clôture) :
//!     bincode(Vec<(décalage, FrameMeta)>) · décalage de l'index (u64) · « STLRIDX\0 »
//! ```
//!
//! Si le fichier n'a pas été clos (plantage), l'index est reconstruit en
//! parcourant les en-têtes d'instantanés, sans décompression.
//!
//! Charge utile d'un instantané (avant compression) :
//! - poids de la poussière (f32), nombre de corps n (u32) ;
//! - boîte de quantification (min x, y, z, côté) et vitesse maximale (f64) ;
//! - identifiants croissants, en écarts codés en varint ;
//! - masses : entières en varint (cas usuel), sinon en f64 ;
//! - positions : 21 bits par axe, rangées par plans d'octets (meilleure compression) ;
//! - vitesses : 16 bits signés par axe, rapportées à la vitesse maximale, par plans d'octets ;
//! - devenirs : (absorbé, résultant) ou (éliminé).

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use stellar_application::{
    BodyFate, FrameMeta, Keyframe, RecordingHeader, RecordingInfo, RecordingReader,
    RecordingRepository, RecordingSummary, RecordingWriter, RepositoryError,
};
use stellar_domain::{BodySet, Vec3};

pub const EXTENSION: &str = "stellarec";
const MAGIC: &[u8; 8] = b"STELREC\0";
const INDEX_MAGIC: &[u8; 8] = b"STLRIDX\0";
const FRAME_MAGIC: u32 = u32::from_le_bytes(*b"FRAM");
const VERSION: u32 = 1;
const ZSTD_LEVEL: i32 = 3;
const POS_BITS: u32 = 21;
const POS_MAX: f64 = ((1u32 << POS_BITS) - 1) as f64;
const VEL_MAX: f64 = 32767.0;
const MAX_HEADER: u32 = 1 << 20;
/// Taille de l'en-tête d'instantané : signature, pas, temps, nombre, taille.
const FRAME_HEAD: u64 = 4 + 8 + 8 + 4 + 4;

fn io(e: std::io::Error) -> RepositoryError {
    RepositoryError::Io(e.to_string())
}

fn fmt(e: impl std::fmt::Display) -> RepositoryError {
    RepositoryError::Format(e.to_string())
}

// ----------------------------------------------------------------------
// Codage d'un instantané
// ----------------------------------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], RepositoryError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| fmt("instantané tronqué"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, RepositoryError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, RepositoryError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 octets"),
        ))
    }
    fn f32(&mut self) -> Result<f32, RepositoryError> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("4 octets"),
        ))
    }
    fn f64(&mut self) -> Result<f64, RepositoryError> {
        Ok(f64::from_le_bytes(
            self.take(8)?.try_into().expect("8 octets"),
        ))
    }
    fn varint(&mut self) -> Result<u64, RepositoryError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(fmt("varint invalide"))
    }
}

/// Encode un instantané (corps triés par identifiant) avant compression.
pub fn encode_frame(frame: &Keyframe) -> Vec<u8> {
    let b = &frame.bodies;
    let n = b.len();
    let mut out = Vec::with_capacity(n * 16 + 64);
    out.extend_from_slice(&frame.dust_weight.to_le_bytes());
    out.extend_from_slice(&(n as u32).to_le_bytes());

    // Boîte de quantification.
    let (mut lo, mut hi) = (Vec3::splat(f64::INFINITY), Vec3::splat(f64::NEG_INFINITY));
    for p in &b.positions {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    if n == 0 {
        lo = Vec3::ZERO;
        hi = Vec3::ONE;
    }
    let size = (hi - lo).max_element().max(1e-12);
    let vmax = b
        .velocities
        .iter()
        .map(|v| v.abs().max_element())
        .fold(0.0f64, f64::max)
        .max(1e-300);
    for x in [lo.x, lo.y, lo.z, size, vmax] {
        out.extend_from_slice(&x.to_le_bytes());
    }

    // Identifiants (écarts).
    let mut prev = 0u64;
    for (k, &id) in b.ids.iter().enumerate() {
        put_varint(&mut out, if k == 0 { id } else { id - prev });
        prev = id;
    }

    // Masses.
    let integral = b
        .masses
        .iter()
        .all(|&m| m >= 0.0 && m.fract() == 0.0 && m < 1e15);
    out.push(if integral { 0 } else { 1 });
    for &m in &b.masses {
        if integral {
            put_varint(&mut out, m as u64);
        } else {
            out.extend_from_slice(&m.to_le_bytes());
        }
    }

    // Positions : 3 axes × 3 plans d'octets.
    for axis in 0..3 {
        let q: Vec<u32> = b
            .positions
            .iter()
            .map(|p| {
                ((((p[axis] - lo[axis]) / size) * POS_MAX)
                    .round()
                    .clamp(0.0, POS_MAX)) as u32
            })
            .collect();
        for plane in 0..3 {
            out.extend(q.iter().map(|v| (v >> (8 * plane)) as u8));
        }
    }

    // Vitesses : 3 axes × 2 plans d'octets.
    for axis in 0..3 {
        let q: Vec<u16> = b
            .velocities
            .iter()
            .map(|v| {
                (((v[axis] / vmax) * VEL_MAX)
                    .round()
                    .clamp(-VEL_MAX, VEL_MAX) as i16) as u16
            })
            .collect();
        for plane in 0..2 {
            out.extend(q.iter().map(|v| (v >> (8 * plane)) as u8));
        }
    }

    // Devenirs.
    out.extend_from_slice(&(frame.fates.len() as u32).to_le_bytes());
    for &(id, fate) in &frame.fates {
        put_varint(&mut out, id);
        match fate {
            BodyFate::MergedInto(s) => {
                out.push(0);
                put_varint(&mut out, s);
            }
            BodyFate::Removed => out.push(1),
        }
    }
    out
}

/// Décode une charge utile (non compressée).
pub fn decode_frame(meta: FrameMeta, buf: &[u8]) -> Result<Keyframe, RepositoryError> {
    let mut c = Cursor { buf, pos: 0 };
    let dust_weight = c.f32()?;
    let n = c.u32()? as usize;
    if n != meta.count as usize {
        return Err(fmt("nombre de corps incohérent"));
    }
    let lo = Vec3::new(c.f64()?, c.f64()?, c.f64()?);
    let size = c.f64()?;
    let vmax = c.f64()?;

    let mut ids = Vec::with_capacity(n);
    let mut prev = 0u64;
    for k in 0..n {
        let d = c.varint()?;
        prev = if k == 0 { d } else { prev + d };
        ids.push(prev);
    }
    let integral = c.u8()? == 0;
    let mut masses = Vec::with_capacity(n);
    for _ in 0..n {
        masses.push(if integral {
            c.varint()? as f64
        } else {
            c.f64()?
        });
    }

    let mut positions = vec![Vec3::ZERO; n];
    for axis in 0..3 {
        let planes = [c.take(n)?, c.take(n)?, c.take(n)?];
        for (i, p) in positions.iter_mut().enumerate() {
            let q = planes[0][i] as u32 | (planes[1][i] as u32) << 8 | (planes[2][i] as u32) << 16;
            p[axis] = lo[axis] + q as f64 / POS_MAX * size;
        }
    }
    let mut velocities = vec![Vec3::ZERO; n];
    for axis in 0..3 {
        let planes = [c.take(n)?, c.take(n)?];
        for (i, v) in velocities.iter_mut().enumerate() {
            let q = (planes[0][i] as u16 | (planes[1][i] as u16) << 8) as i16;
            v[axis] = q as f64 / VEL_MAX * vmax;
        }
    }

    let nf = c.u32()? as usize;
    let mut fates = Vec::with_capacity(nf);
    for _ in 0..nf {
        let id = c.varint()?;
        let fate = match c.u8()? {
            0 => BodyFate::MergedInto(c.varint()?),
            1 => BodyFate::Removed,
            _ => return Err(fmt("devenir inconnu")),
        };
        fates.push((id, fate));
    }
    Ok(Keyframe {
        step: meta.step,
        time: meta.time,
        bodies: BodySet {
            ids,
            positions,
            velocities,
            masses,
        },
        fates,
        dust_weight,
    })
}

// ----------------------------------------------------------------------
// Écriture
// ----------------------------------------------------------------------

pub struct FileRecordingWriter {
    path: PathBuf,
    out: BufWriter<File>,
    offset: u64,
    index: Vec<(u64, FrameMeta)>,
}

impl RecordingWriter for FileRecordingWriter {
    fn write_frame(&mut self, frame: &Keyframe) -> Result<u64, RepositoryError> {
        let payload = zstd::bulk::compress(&encode_frame(frame), ZSTD_LEVEL).map_err(io)?;
        let meta = FrameMeta {
            step: frame.step,
            time: frame.time,
            count: frame.bodies.len() as u32,
        };
        let w = &mut self.out;
        w.write_all(&FRAME_MAGIC.to_le_bytes()).map_err(io)?;
        w.write_all(&meta.step.to_le_bytes()).map_err(io)?;
        w.write_all(&meta.time.to_le_bytes()).map_err(io)?;
        w.write_all(&meta.count.to_le_bytes()).map_err(io)?;
        w.write_all(&(payload.len() as u32).to_le_bytes())
            .map_err(io)?;
        w.write_all(&payload).map_err(io)?;
        self.index.push((self.offset, meta));
        let written = FRAME_HEAD + payload.len() as u64;
        self.offset += written;
        Ok(written)
    }

    fn finish(mut self: Box<Self>) -> Result<PathBuf, RepositoryError> {
        let index = bincode::serialize(&self.index).map_err(fmt)?;
        let w = &mut self.out;
        w.write_all(&index).map_err(io)?;
        w.write_all(&self.offset.to_le_bytes()).map_err(io)?;
        w.write_all(INDEX_MAGIC).map_err(io)?;
        w.flush().map_err(io)?;
        Ok(self.path.clone())
    }
}

// ----------------------------------------------------------------------
// Lecture
// ----------------------------------------------------------------------

pub struct FileRecordingReader {
    file: BufReader<File>,
    info: RecordingInfo,
    offsets: Vec<u64>,
    complete: bool,
}

fn read_exact<const N: usize>(r: &mut impl Read) -> Result<[u8; N], RepositoryError> {
    let mut b = [0u8; N];
    r.read_exact(&mut b).map_err(|_| fmt("fichier tronqué"))?;
    Ok(b)
}

impl FileRecordingReader {
    pub fn open(path: &Path) -> Result<Self, RepositoryError> {
        let mut file = BufReader::new(File::open(path).map_err(io)?);
        if &read_exact::<8>(&mut file)? != MAGIC {
            return Err(fmt("ce n'est pas un enregistrement Stellar"));
        }
        let version = u32::from_le_bytes(read_exact::<4>(&mut file)?);
        if version != VERSION {
            return Err(RepositoryError::UnsupportedVersion(version));
        }
        let len = u32::from_le_bytes(read_exact::<4>(&mut file)?);
        if len > MAX_HEADER {
            return Err(fmt("en-tête démesuré"));
        }
        let mut hb = vec![0u8; len as usize];
        file.read_exact(&mut hb).map_err(io)?;
        let header: RecordingHeader = bincode::deserialize(&hb).map_err(fmt)?;
        let data_start = 16 + len as u64;
        let file_len = file.get_ref().metadata().map_err(io)?.len();

        let (index, complete) = match Self::read_index(&mut file, file_len) {
            Some(index) => (index, true),
            None => (Self::scan(&mut file, data_start, file_len)?, false),
        };
        let offsets = index.iter().map(|e| e.0).collect();
        let frames = index.into_iter().map(|e| e.1).collect();
        Ok(Self {
            file,
            info: RecordingInfo { header, frames },
            offsets,
            complete,
        })
    }

    fn read_index(file: &mut BufReader<File>, file_len: u64) -> Option<Vec<(u64, FrameMeta)>> {
        if file_len < 16 {
            return None;
        }
        file.seek(SeekFrom::Start(file_len - 16)).ok()?;
        let offset = u64::from_le_bytes(read_exact::<8>(file).ok()?);
        if &read_exact::<8>(file).ok()? != INDEX_MAGIC || offset > file_len - 16 {
            return None;
        }
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; (file_len - 16 - offset) as usize];
        file.read_exact(&mut buf).ok()?;
        bincode::deserialize(&buf).ok()
    }

    /// Reconstruit l'index d'un fichier non clos ; le dernier instantané,
    /// s'il est incomplet, est ignoré.
    fn scan(
        file: &mut BufReader<File>,
        start: u64,
        file_len: u64,
    ) -> Result<Vec<(u64, FrameMeta)>, RepositoryError> {
        let mut index = Vec::new();
        let mut pos = start;
        while pos + FRAME_HEAD <= file_len {
            file.seek(SeekFrom::Start(pos)).map_err(io)?;
            let Ok(magic) = read_exact::<4>(file) else {
                break;
            };
            if u32::from_le_bytes(magic) != FRAME_MAGIC {
                break;
            }
            let step = u64::from_le_bytes(read_exact::<8>(file)?);
            let time = f64::from_le_bytes(read_exact::<8>(file)?);
            let count = u32::from_le_bytes(read_exact::<4>(file)?);
            let size = u32::from_le_bytes(read_exact::<4>(file)?) as u64;
            if pos + FRAME_HEAD + size > file_len {
                break;
            }
            index.push((pos, FrameMeta { step, time, count }));
            pos += FRAME_HEAD + size;
        }
        Ok(index)
    }
}

impl RecordingReader for FileRecordingReader {
    fn info(&self) -> &RecordingInfo {
        &self.info
    }

    fn read_frame(&mut self, index: usize) -> Result<Keyframe, RepositoryError> {
        let offset = *self
            .offsets
            .get(index)
            .ok_or_else(|| fmt("instantané hors limites"))?;
        let meta = self.info.frames[index];
        self.file
            .seek(SeekFrom::Start(offset + FRAME_HEAD - 4))
            .map_err(io)?;
        let size = u32::from_le_bytes(read_exact::<4>(&mut self.file)?) as usize;
        let mut payload = vec![0u8; size];
        self.file.read_exact(&mut payload).map_err(io)?;
        // Taille décompressée bornée : ≈ 30 octets par corps au plus.
        let cap = meta.count as usize * 40 + (1 << 20);
        let raw = zstd::bulk::decompress(&payload, cap).map_err(fmt)?;
        decode_frame(meta, &raw)
    }
}

// ----------------------------------------------------------------------
// Dépôt
// ----------------------------------------------------------------------

/// Enregistrements rangés dans un dossier.
#[derive(Debug, Clone)]
pub struct FileRecordingRepository {
    dir: PathBuf,
}

impl FileRecordingRepository {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
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
        "enregistrement".to_owned()
    } else {
        s
    }
}

impl RecordingRepository for FileRecordingRepository {
    fn create(
        &self,
        name: &str,
        header: &RecordingHeader,
    ) -> Result<Box<dyn RecordingWriter>, RepositoryError> {
        fs::create_dir_all(&self.dir).map_err(io)?;
        let stem = sanitize(name);
        let mut path = self.dir.join(format!("{stem}.{EXTENSION}"));
        let mut k = 2;
        while path.exists() {
            path = self.dir.join(format!("{stem}_{k}.{EXTENSION}"));
            k += 1;
        }
        let hb = bincode::serialize(header).map_err(fmt)?;
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&path).map_err(io)?);
        out.write_all(MAGIC).map_err(io)?;
        out.write_all(&VERSION.to_le_bytes()).map_err(io)?;
        out.write_all(&(hb.len() as u32).to_le_bytes())
            .map_err(io)?;
        out.write_all(&hb).map_err(io)?;
        let offset = 16 + hb.len() as u64;
        Ok(Box::new(FileRecordingWriter {
            path,
            out,
            offset,
            index: Vec::new(),
        }))
    }

    fn open(&self, path: &Path) -> Result<Box<dyn RecordingReader>, RepositoryError> {
        Ok(Box::new(FileRecordingReader::open(path)?))
    }

    fn list(&self) -> Result<Vec<RecordingSummary>, RepositoryError> {
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
            let Ok(r) = FileRecordingReader::open(&path) else {
                continue;
            };
            let frames = &r.info.frames;
            out.push(RecordingSummary {
                name: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                created_at_unix: r.info.header.created_at_unix,
                frame_count: frames.len(),
                first_count: frames.first().map_or(0, |f| f.count),
                duration: match (frames.first(), frames.last()) {
                    (Some(a), Some(b)) => b.time - a.time,
                    _ => 0.0,
                },
                bytes: entry.metadata().map(|m| m.len()).unwrap_or(0),
                complete: r.complete,
                path,
            });
        }
        out.sort_by(|a, b| {
            b.created_at_unix
                .cmp(&a.created_at_unix)
                .then(a.name.cmp(&b.name))
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_domain::SimulationConfig;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("stellar-rec-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn frame(k: u64) -> Keyframe {
        let mut b = BodySet::default();
        for id in 0..5_000u64 {
            if id % 7 == 3 && k > 0 {
                continue; // absorbés après le premier instantané
            }
            let f = id as f64 * 0.01 + k as f64;
            b.push(
                id * 3,
                Vec3::new(f.sin() * 50.0, f.cos() * 20.0, f * 0.1),
                Vec3::new(-f, 0.5, f.sin()),
                if id % 7 == 0 { 2.0 } else { 1.0 },
            );
        }
        let fates = if k == 1 {
            (0..5_000u64)
                .filter(|i| i % 7 == 3)
                .map(|i| (i * 3, BodyFate::MergedInto(0)))
                .collect()
        } else {
            vec![]
        };
        Keyframe {
            step: k * 10,
            time: k as f64 * 0.5,
            bodies: b,
            fates,
            dust_weight: 1.5,
        }
    }

    fn header() -> RecordingHeader {
        RecordingHeader {
            config: SimulationConfig::default(),
            interval: 10,
            start_step: 0,
            created_at_unix: 42,
            dust_budget: Some(1000),
        }
    }

    #[test]
    fn encode_decode_is_accurate() {
        let f = frame(1);
        let meta = FrameMeta {
            step: f.step,
            time: f.time,
            count: f.bodies.len() as u32,
        };
        let d = decode_frame(meta, &encode_frame(&f)).unwrap();
        assert_eq!(d.bodies.ids, f.bodies.ids);
        assert_eq!(d.bodies.masses, f.bodies.masses);
        assert_eq!(d.fates, f.fates);
        assert_eq!(d.dust_weight, 1.5);
        let size = 100.0f64; // ordre de grandeur de la boîte
        for (a, b) in d.bodies.positions.iter().zip(&f.bodies.positions) {
            assert!((*a - *b).abs().max_element() <= size / POS_MAX);
        }
        let vmax = f
            .bodies
            .velocities
            .iter()
            .map(|v| v.abs().max_element())
            .fold(0.0, f64::max);
        for (a, b) in d.bodies.velocities.iter().zip(&f.bodies.velocities) {
            assert!((*a - *b).abs().max_element() <= vmax / VEL_MAX);
        }
    }

    #[test]
    fn write_list_and_read_back() {
        let dir = temp_dir("rw");
        let repo = FileRecordingRepository::new(&dir);
        let mut w = repo.create("essai", &header()).unwrap();
        for k in 0..4 {
            w.write_frame(&frame(k)).unwrap();
        }
        let path = w.finish().unwrap();
        let list = repo.list().unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].complete);
        assert_eq!(list[0].frame_count, 4);
        assert!((list[0].duration - 1.5).abs() < 1e-12);

        let mut r = repo.open(&path).unwrap();
        assert_eq!(r.info().header, header());
        let f2 = r.read_frame(2).unwrap();
        assert_eq!(f2.step, 20);
        assert_eq!(f2.bodies.len(), frame(2).bodies.len());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unfinished_file_is_recovered() {
        let dir = temp_dir("crash");
        let repo = FileRecordingRepository::new(&dir);
        let mut w = repo.create("crash", &header()).unwrap();
        for k in 0..3 {
            w.write_frame(&frame(k)).unwrap();
        }
        drop(w); // pas de finish : index absent
        let list = repo.list().unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].complete);
        assert_eq!(list[0].frame_count, 3);
        let mut r = repo.open(&list[0].path).unwrap();
        assert_eq!(r.read_frame(2).unwrap().step, 20);
        let _ = fs::remove_dir_all(&dir);
    }
}
