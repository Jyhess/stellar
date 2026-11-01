//! `.stellarec` recording format (version 5; versions 1 to 4 are read:
//! headers of versions 1 to 4 in [`crate::legacy`], snapshots of versions 1
//! and 2 without subsystems; a resumed recording keeps the version of its
//! file). Layout and history: `docs/formats.md`.
//!
//! ```text
//! signature "STELREC\0" · version (u32) · size L (u32) · bincode header (L bytes)
//! snapshot*:
//!     "FRAM" (u32) · step (u64) · time (f64) · body count (u32)
//!     size C (u32) · zstd-compressed payload (C bytes)
//! index (at the end of the file, written when closing):
//!     bincode(Vec<(offset, FrameMeta)>) · index offset (u64) · "STLRIDX\0"
//! ```
//!
//! If the file was not closed (crash), the index is rebuilt by walking the
//! snapshot headers, without decompression.
//!
//! Payload of a snapshot (before compression):
//! - dust weight (f32), body count n (u32);
//! - number G of subsystems (u32, version 3 and later), then G quantization
//!   boxes (min x, y, z, side, f64) and the maximum speed (f64); versions 1
//!   and 2: one box, then the maximum speed;
//! - increasing identifiers, as gaps encoded as varints;
//! - masses: integers as varints (the usual case), otherwise f64;
//! - if G > 1: subsystem of each body (u16, two byte planes);
//! - positions: 21 bits per axis, within the box of their subsystem, stored
//!   as byte planes (better compression);
//! - velocities: 16 signed bits per axis, relative to the maximum speed, as
//!   byte planes;
//! - fates: (absorbed, resulting) or (removed).

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use stellar_application::{
    BodyFate, FormatError, FrameMeta, Keyframe, RecordingHeader, RecordingInfo, RecordingReader,
    RecordingRepository, RecordingSummary, RecordingWriter, RepositoryError,
};
use stellar_domain::{BodySet, Vec3};

pub const EXTENSION: &str = "stellarec";
const MAGIC: &[u8; 8] = b"STELREC\0";
const INDEX_MAGIC: &[u8; 8] = b"STLRIDX\0";
const FRAME_MAGIC: u32 = u32::from_le_bytes(*b"FRAM");
/// Version written; versions 1 (parameters without ejected clusters), 2
/// (snapshots without subsystems), 3 (orbital velocity without factor) and
/// 4 (dust of mass 1, no sun, no planets) are read. Snapshots of versions 3
/// to 5 have the same layout.
pub const VERSION: u32 = 5;
const ZSTD_LEVEL: i32 = 3;
const POS_BITS: u32 = 21;
const POS_MAX: f64 = ((1u32 << POS_BITS) - 1) as f64;
const VEL_MAX: f64 = 32767.0;
const MAX_HEADER: u32 = 1 << 20;
/// Size of a snapshot header: signature, step, time, count, size.
const FRAME_HEAD: u64 = 4 + 8 + 8 + 4 + 4;

fn io(e: std::io::Error) -> RepositoryError {
    RepositoryError::Io(e.to_string())
}

/// Decoding failure of the serializer.
fn fmt(e: impl std::fmt::Display) -> RepositoryError {
    FormatError::Decode(e.to_string()).into()
}

// ----------------------------------------------------------------------
// Snapshot encoding
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
            .ok_or(FormatError::TruncatedSnapshot)?;
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
        Err(FormatError::InvalidVarint.into())
    }
}

/// Quantization boxes (corner, side) of the `count` groups.
fn boxes(positions: &[Vec3], group: impl Fn(usize) -> usize, count: usize) -> Vec<(Vec3, f64)> {
    let mut lo = vec![Vec3::splat(f64::INFINITY); count];
    let mut hi = vec![Vec3::splat(f64::NEG_INFINITY); count];
    for (i, p) in positions.iter().enumerate() {
        let g = group(i);
        lo[g] = lo[g].min(*p);
        hi[g] = hi[g].max(*p);
    }
    lo.into_iter()
        .zip(hi)
        .map(|(lo, hi)| {
            if lo.x.is_finite() {
                (lo, (hi - lo).max_element().max(1e-12))
            } else {
                (Vec3::ZERO, 1.0)
            }
        })
        .collect()
}

/// Encodes a snapshot (bodies sorted by identifier), before compression, in
/// the layout of version `version` of the file (a resumed recording keeps
/// its own).
pub fn encode_frame(frame: &Keyframe, version: u32) -> Vec<u8> {
    let b = &frame.bodies;
    let n = b.len();
    let mut out = Vec::with_capacity(n * 16 + 64);
    out.extend_from_slice(&frame.dust_weight.to_le_bytes());
    out.extend_from_slice(&(n as u32).to_le_bytes());

    // Quantization boxes: one per subsystem (version 3 and later).
    let grouped = version >= 3 && frame.groups.len() == n && n > 0;
    let count = if grouped {
        frame.groups.iter().copied().max().unwrap_or(0) as usize + 1
    } else {
        1
    };
    let group = |i: usize| if grouped { frame.groups[i] as usize } else { 0 };
    let boxes = boxes(&b.positions, group, count);
    let vmax = b
        .velocities
        .iter()
        .map(|v| v.abs().max_element())
        .fold(0.0f64, f64::max)
        .max(1e-300);
    if version >= 3 {
        out.extend_from_slice(&(count as u32).to_le_bytes());
    }
    for (lo, size) in &boxes {
        for x in [lo.x, lo.y, lo.z, *size] {
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    out.extend_from_slice(&vmax.to_le_bytes());

    // Identifiers (gaps).
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

    // Subsystems: 2 byte planes.
    if count > 1 {
        for plane in 0..2 {
            out.extend(frame.groups.iter().map(|g| (g >> (8 * plane)) as u8));
        }
    }

    // Positions: 3 axes × 3 byte planes.
    for axis in 0..3 {
        let q: Vec<u32> = b
            .positions
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (lo, size) = boxes[group(i)];
                ((((p[axis] - lo[axis]) / size) * POS_MAX)
                    .round()
                    .clamp(0.0, POS_MAX)) as u32
            })
            .collect();
        for plane in 0..3 {
            out.extend(q.iter().map(|v| (v >> (8 * plane)) as u8));
        }
    }

    // Velocities: 3 axes × 2 byte planes.
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

    // Fates.
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

/// Decodes an (uncompressed) payload of a file of version `version`.
pub fn decode_frame(meta: FrameMeta, buf: &[u8], version: u32) -> Result<Keyframe, RepositoryError> {
    let mut c = Cursor { buf, pos: 0 };
    let dust_weight = c.f32()?;
    let n = c.u32()? as usize;
    if n != meta.count as usize {
        return Err(FormatError::InconsistentBodyCount.into());
    }
    let count = if version >= 3 { c.u32()? as usize } else { 1 };
    if count == 0 || count > (1 << 16) {
        return Err(FormatError::InvalidSubsystemCount.into());
    }
    let mut boxes = Vec::with_capacity(count);
    for _ in 0..count {
        boxes.push((Vec3::new(c.f64()?, c.f64()?, c.f64()?), c.f64()?));
    }
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

    let mut groups = Vec::new();
    if count > 1 {
        let planes = [c.take(n)?, c.take(n)?];
        groups = (0..n)
            .map(|i| planes[0][i] as u16 | (planes[1][i] as u16) << 8)
            .collect();
        if groups.iter().any(|&g| g as usize >= count) {
            return Err(FormatError::UnknownSubsystem.into());
        }
    }

    let mut positions = vec![Vec3::ZERO; n];
    for axis in 0..3 {
        let planes = [c.take(n)?, c.take(n)?, c.take(n)?];
        for (i, p) in positions.iter_mut().enumerate() {
            let (lo, size) = boxes[groups.get(i).copied().unwrap_or(0) as usize];
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
            _ => return Err(FormatError::UnknownFate.into()),
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
        groups,
    })
}

// ----------------------------------------------------------------------
// Writing
// ----------------------------------------------------------------------

pub struct FileRecordingWriter {
    path: PathBuf,
    /// File version (that of a resumed recording is kept).
    version: u32,
    out: BufWriter<File>,
    offset: u64,
    index: Vec<(u64, FrameMeta)>,
}

impl RecordingWriter for FileRecordingWriter {
    fn path(&self) -> PathBuf {
        self.path.clone()
    }

    fn write_frame(&mut self, frame: &Keyframe) -> Result<u64, RepositoryError> {
        let payload =
            zstd::bulk::compress(&encode_frame(frame, self.version), ZSTD_LEVEL).map_err(io)?;
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
// Reading
// ----------------------------------------------------------------------

pub struct FileRecordingReader {
    file: BufReader<File>,
    version: u32,
    info: RecordingInfo,
    offsets: Vec<u64>,
    complete: bool,
}

fn read_exact<const N: usize>(r: &mut impl Read) -> Result<[u8; N], RepositoryError> {
    let mut b = [0u8; N];
    r.read_exact(&mut b).map_err(|_| FormatError::Truncated)?;
    Ok(b)
}

impl FileRecordingReader {
    pub fn open(path: &Path) -> Result<Self, RepositoryError> {
        let mut file = BufReader::new(File::open(path).map_err(io)?);
        if &read_exact::<8>(&mut file)? != MAGIC {
            return Err(FormatError::NotARecording.into());
        }
        let version = u32::from_le_bytes(read_exact::<4>(&mut file)?);
        if !(1..=VERSION).contains(&version) {
            return Err(RepositoryError::UnsupportedVersion(version));
        }
        let len = u32::from_le_bytes(read_exact::<4>(&mut file)?);
        if len > MAX_HEADER {
            return Err(FormatError::OversizedHeader.into());
        }
        let mut hb = vec![0u8; len as usize];
        file.read_exact(&mut hb).map_err(io)?;
        let header: RecordingHeader = if version == 1 {
            bincode::deserialize::<crate::legacy::RecordingHeaderV1>(&hb)
                .map_err(fmt)?
                .into()
        } else if version <= 3 {
            bincode::deserialize::<crate::legacy::RecordingHeaderV3>(&hb)
                .map_err(fmt)?
                .into()
        } else if version == 4 {
            bincode::deserialize::<crate::legacy::RecordingHeaderV4>(&hb)
                .map_err(fmt)?
                .into()
        } else {
            bincode::deserialize(&hb).map_err(fmt)?
        };
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
            version,
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

    /// Rebuilds the index of a file that was not closed; the last snapshot, if
    /// incomplete, is ignored.
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
            .ok_or(FormatError::SnapshotOutOfRange)?;
        let meta = self.info.frames[index];
        self.file
            .seek(SeekFrom::Start(offset + FRAME_HEAD - 4))
            .map_err(io)?;
        let size = u32::from_le_bytes(read_exact::<4>(&mut self.file)?) as usize;
        let mut payload = vec![0u8; size];
        self.file.read_exact(&mut payload).map_err(io)?;
        // Bound on the decompressed size: at most ~35 bytes per body plus the fates.
        let cap = meta.count as usize * 40 + (1 << 20);
        let raw = zstd::bulk::decompress(&payload, cap).map_err(fmt)?;
        decode_frame(meta, &raw, self.version)
    }
}

// ----------------------------------------------------------------------
// Repository
// ----------------------------------------------------------------------

/// Recordings stored in a folder.
#[derive(Debug, Clone)]
pub struct FileRecordingRepository {
    dir: PathBuf,
    /// Deletion to the recycle bin (otherwise permanent).
    trash: bool,
}

impl FileRecordingRepository {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            trash: false,
        }
    }

    /// Deletions go through the Windows recycle bin.
    pub fn with_trash(mut self) -> Self {
        self.trash = true;
        self
    }
}


impl RecordingRepository for FileRecordingRepository {
    fn create(
        &self,
        name: &str,
        header: &RecordingHeader,
    ) -> Result<Box<dyn RecordingWriter>, RepositoryError> {
        fs::create_dir_all(&self.dir).map_err(io)?;
        let stem = crate::files::sanitize(name, "enregistrement");
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
            version: VERSION,
            out,
            offset,
            index: Vec::new(),
        }))
    }

    fn open(&self, path: &Path) -> Result<Box<dyn RecordingReader>, RepositoryError> {
        Ok(Box::new(FileRecordingReader::open(path)?))
    }

    fn resume(
        &self,
        path: &Path,
        keep_through_step: u64,
    ) -> Result<Box<dyn RecordingWriter>, RepositoryError> {
        let mut reader = FileRecordingReader::open(path)?;
        let frames = &reader.info.frames;
        // Kept snapshots: those up to the resume step, included.
        let keep = frames.partition_point(|f| f.step <= keep_through_step);
        if keep == 0 || frames[keep - 1].step != keep_through_step {
            return Err(FormatError::MissingSnapshot(keep_through_step).into());
        }
        // End of the last kept snapshot: everything after it (later snapshots,
        // old index) is truncated.
        let last = reader.offsets[keep - 1];
        let version = reader.version;
        reader
            .file
            .seek(SeekFrom::Start(last + FRAME_HEAD - 4))
            .map_err(io)?;
        let size = u32::from_le_bytes(read_exact::<4>(&mut reader.file)?) as u64;
        let end = last + FRAME_HEAD + size;
        let index: Vec<(u64, FrameMeta)> = reader
            .offsets
            .iter()
            .copied()
            .zip(reader.info.frames.iter().copied())
            .take(keep)
            .collect();
        drop(reader);

        let file = fs::OpenOptions::new().write(true).open(path).map_err(io)?;
        file.set_len(end).map_err(io)?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        out.seek(SeekFrom::Start(end)).map_err(io)?;
        Ok(Box::new(FileRecordingWriter {
            path: path.to_path_buf(),
            version,
            out,
            offset: end,
            index,
        }))
    }

    fn delete(&self, path: &Path) -> Result<(), RepositoryError> {
        crate::files::delete(path, self.trash)
    }

    fn rename(&self, path: &Path, name: &str) -> Result<PathBuf, RepositoryError> {
        crate::files::rename(path, name, EXTENSION, "enregistrement")
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
                continue; // absorbed after the first snapshot
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
            groups: Vec::new(),
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
        let d = decode_frame(meta, &encode_frame(&f, VERSION), VERSION).unwrap();
        assert_eq!(d.bodies.ids, f.bodies.ids);
        assert_eq!(d.bodies.masses, f.bodies.masses);
        assert_eq!(d.fates, f.fates);
        assert_eq!(d.dust_weight, 1.5);
        let size = 100.0f64; // order of magnitude of the box
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
    fn subsystems_keep_their_own_precision() {
        // A compact cluster gone very far away: with one box per subsystem,
        // neither it nor the main system loses precision.
        let mut f = frame(0);
        let n = f.bodies.len();
        f.groups = vec![0; n];
        for i in (0..n).step_by(50) {
            f.bodies.positions[i] = Vec3::new(1e7, 0.0, 0.0) + f.bodies.positions[i] * 1e-3;
            f.groups[i] = 1;
        }
        let meta = FrameMeta {
            step: f.step,
            time: f.time,
            count: n as u32,
        };
        let d = decode_frame(meta, &encode_frame(&f, VERSION), VERSION).unwrap();
        assert_eq!(d.groups, f.groups);
        for (i, (a, b)) in d.bodies.positions.iter().zip(&f.bodies.positions).enumerate() {
            let tol = if f.groups[i] == 1 { 0.2 } else { 100.0 } / POS_MAX;
            assert!((*a - *b).abs().max_element() <= tol, "{a} ≠ {b}");
        }
        // Layout of older versions: a single box, no subsystems read back.
        let old = decode_frame(meta, &encode_frame(&f, 2), 2).unwrap();
        assert!(old.groups.is_empty());
        assert_eq!(old.bodies.ids, f.bodies.ids);
    }

    #[test]
    fn write_list_and_read_back() {
        let dir = temp_dir("rw");
        let repo = FileRecordingRepository::new(&dir);
        let mut w = repo.create("test", &header()).unwrap();
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
    fn reads_version_1() {
        let dir = temp_dir("v1");
        let repo = FileRecordingRepository::new(&dir);
        // Version 1 file: old header, snapshots in the old layout, no index
        // (rebuilt on opening).
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.stellarec");
        let mut frames = Vec::new();
        for k in 0..3 {
            let f = frame(k);
            let payload = zstd::bulk::compress(&encode_frame(&f, 1), ZSTD_LEVEL).unwrap();
            frames.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
            frames.extend_from_slice(&f.step.to_le_bytes());
            frames.extend_from_slice(&f.time.to_le_bytes());
            frames.extend_from_slice(&(f.bodies.len() as u32).to_le_bytes());
            frames.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            frames.extend_from_slice(&payload);
        }
        let h = header();
        let p = h.config.physics;
        let old = crate::legacy::RecordingHeaderV1 {
            config: crate::legacy::SimulationConfigV3 {
                initial: h.config.initial.into(),
                physics: crate::legacy::PhysicsParamsV3 {
                    g: p.g,
                    dt: p.dt,
                    softening: p.softening,
                    theta: p.theta,
                    base_radius: p.base_radius,
                    cleanup_period: p.cleanup_period,
                    cleanup_distance_factor: p.cleanup_distance_factor,
                    reference_radius: p.reference_radius,
                },
                engine: h.config.engine.clone(),
            },
            interval: h.interval,
            start_step: h.start_step,
            created_at_unix: h.created_at_unix,
            dust_budget: h.dust_budget,
        };
        let hb = bincode::serialize(&old).unwrap();
        let mut v1 = Vec::new();
        v1.extend_from_slice(MAGIC);
        v1.extend_from_slice(&1u32.to_le_bytes());
        v1.extend_from_slice(&(hb.len() as u32).to_le_bytes());
        v1.extend_from_slice(&hb);
        v1.extend_from_slice(&frames);
        fs::write(&path, v1).unwrap();

        let mut r = repo.open(&path).unwrap();
        assert_eq!(r.info().header, header());
        assert_eq!(r.info().frames.len(), 3);
        assert_eq!(r.read_frame(2).unwrap().step, 20);
        drop(r);

        // Version 3: header without orbital velocity factor (1).
        let old = crate::legacy::RecordingHeaderV3 {
            config: crate::legacy::SimulationConfigV5 {
                initial: h.config.initial.into(),
                physics: h.config.physics,
                engine: h.config.engine.clone(),
            },
            interval: h.interval,
            start_step: h.start_step,
            created_at_unix: h.created_at_unix,
            dust_budget: h.dust_budget,
        };
        let hb = bincode::serialize(&old).unwrap();
        let mut v3 = Vec::new();
        v3.extend_from_slice(MAGIC);
        v3.extend_from_slice(&3u32.to_le_bytes());
        v3.extend_from_slice(&(hb.len() as u32).to_le_bytes());
        v3.extend_from_slice(&hb);
        fs::write(&path, v3).unwrap();
        let r = repo.open(&path).unwrap();
        assert_eq!(r.info().header, header());
        drop(r);

        // Version 4: dust of mass 1, no sun, no planets.
        let old = crate::legacy::RecordingHeaderV4 {
            config: crate::legacy::SimulationConfigV6 {
                initial: h.config.initial.into(),
                physics: h.config.physics,
                engine: h.config.engine.clone(),
            },
            interval: h.interval,
            start_step: h.start_step,
            created_at_unix: h.created_at_unix,
            dust_budget: h.dust_budget,
        };
        let hb = bincode::serialize(&old).unwrap();
        let mut v4 = Vec::new();
        v4.extend_from_slice(MAGIC);
        v4.extend_from_slice(&4u32.to_le_bytes());
        v4.extend_from_slice(&(hb.len() as u32).to_le_bytes());
        v4.extend_from_slice(&hb);
        fs::write(&path, v4).unwrap();
        let r = repo.open(&path).unwrap();
        assert_eq!(r.info().header, header());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_truncates_and_appends() {
        let dir = temp_dir("resume");
        let repo = FileRecordingRepository::new(&dir);
        let mut w = repo.create("next", &header()).unwrap();
        for k in 0..5 {
            w.write_frame(&frame(k)).unwrap(); // steps 0, 10, 20, 30, 40
        }
        let path = w.finish().unwrap();

        // Resume at step 20: snapshots 30 and 40 disappear.
        let mut w = repo.resume(&path, 20).unwrap();
        let mut next = frame(3);
        next.step = 25;
        w.write_frame(&next).unwrap();
        w.finish().unwrap();
        let mut r = repo.open(&path).unwrap();
        let steps: Vec<u64> = r.info().frames.iter().map(|f| f.step).collect();
        assert_eq!(steps, vec![0, 10, 20, 25]);
        assert_eq!(r.read_frame(3).unwrap().step, 25);
        assert_eq!(r.read_frame(2).unwrap().bodies, {
            let mut f = frame(2);
            let d = decode_frame(
                FrameMeta {
                    step: 20,
                    time: 1.0,
                    count: f.bodies.len() as u32,
                },
                &encode_frame(&f, VERSION),
                VERSION,
            )
            .unwrap();
            f.bodies = d.bodies;
            f.bodies
        });

        // Resume at a missing step: refused.
        assert!(repo.resume(&path, 21).is_err());

        // Resume of a file that was not closed.
        let mut w = repo.resume(&path, 25).unwrap();
        let mut more = frame(4);
        more.step = 35;
        w.write_frame(&more).unwrap();
        drop(w);
        let r = repo.open(&path).unwrap();
        let steps: Vec<u64> = r.info().frames.iter().map(|f| f.step).collect();
        assert_eq!(steps, vec![0, 10, 20, 25, 35]);
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
        drop(w); // no finish: no index
        let list = repo.list().unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].complete);
        assert_eq!(list[0].frame_count, 3);
        let mut r = repo.open(&list[0].path).unwrap();
        assert_eq!(r.read_frame(2).unwrap().step, 20);
        let _ = fs::remove_dir_all(&dir);
    }
}
