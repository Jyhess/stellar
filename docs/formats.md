# File formats

Stellar writes two kinds of files, both handled by `crates/stellar-persistence`:

| Extension | Content | Default folder | Code |
|---|---|---|---|
| `.stellar` | save: full state, parameters, camera, recording links | `saves\` | `src/lib.rs` |
| `.stellarec` | recording: one compressed snapshot every N steps | `recordings\` | `src/recording.rs` |

Older versions are read through copies of their former structures in `src/legacy.rs`. Both folders are relative to the current directory.

All integers are little-endian. Structures marked *bincode* use the standard `bincode` encoding of the Rust types. That encoding is **not self-describing**: adding, removing or reordering a field of any serialized type changes the bytes. Such a change therefore requires a new format version (see `.claude/skills/file-format/SKILL.md`).

---

## Saves (`.stellar`, version 6)

| Bytes | Content |
|---|---|
| 8 | signature `STELLAR\0` |
| 4 | format version (`u32`) |
| 4 | header size L (`u32`, at most 1 MiB) |
| L | header: `SaveSummary`, bincode |
| rest | `SaveData`, bincode, compressed with zstd (level 3) |

The home screen lists saves by reading the header alone, without decompressing anything.

- **`SaveSummary`** (`stellar-application/src/ports.rs`): name of origin, save date (Unix seconds, UTC), body count, step, simulated time, engine identifier, name of the linked recording. The displayed name is the file name; the header keeps the name the save was created with.
- **`SaveData`**:
  - `config: SimulationConfig`: initial conditions and `PhysicsParams`;
  - `state: SimulationState`: bodies in global coordinates, grouped by subsystem (`systems: Vec<SystemSpan>`, range and size of each);
  - `view: Option<ViewState>`: camera and selection;
  - `recording: Option<RecordingLink>`: suspended recording, resumed when the save is loaded;
  - `continue_recording: Option<PathBuf>`: recording that was running when F5 was pressed. It is continued on load, and the snapshots after the save's step are deleted.

### Version history

| Version | Change | Reading of older versions |
|---|---|---|
| 3 | camera roll, barycenter tracking, recording continuation | thresholds of ejected clusters filled with the suggested values; a single system |
| 4 | thresholds of ejected clusters (`cluster_link_length`, `cluster_min_bodies`, `cluster_min_mass`) | a single system |
| 5 | subsystems in the state (`SystemSpan`) | orbital velocity factor = 1 |
| 6 | orbital velocity factor (`VelocityMode::CircularOrbital { spread, factor }`) | current version |

Versions 1 and 2 are refused.

---

## Recordings (`.stellarec`, version 4)

```text
signature "STELREC\0" · version (u32) · header size L (u32) · header, bincode (L bytes)
snapshot*:
    "FRAM" (u32) · step (u64) · time (f64) · body count (u32)
    payload size C (u32) · payload compressed with zstd (C bytes)
index (end of file, written when the recording is closed):
    bincode(Vec<(offset, FrameMeta)>) · index offset (u64) · "STLRIDX\0"
```

- **Header** (`RecordingHeader`, `stellar-application/src/recording.rs`): `SimulationConfig` (initial conditions and physics parameters), snapshot interval in steps, first step, creation date, dust budget (`None` means every body).
- **Snapshot header**: 28 bytes. This is enough to rebuild the index without decompressing anything when the file was not closed, for example after a crash.

### Snapshot payload (before compression)

1. dust weight (`f32`), body count n (`u32`);
2. number G of subsystems (`u32`), then G quantization boxes (min x, y, z and side, `f64`), then the maximum speed (`f64`);
3. identifiers in increasing order, stored as differences encoded as varints;
4. masses: as integers encoded as varints (the usual case), otherwise as `f64`;
5. if G > 1, the subsystem of each body (`u16`, stored as two byte planes);
6. positions: 21 bits per axis within the box of the body's subsystem, stored as byte planes, which compress better;
7. velocities: 16 signed bits per axis, relative to the maximum speed, stored as byte planes;
8. fates of the bodies that disappeared since the previous snapshot: (absorbed, resulting body) or (eliminated).

A snapshot weighs about 12 bytes per recorded body once compressed. With a dust budget B, at most B bodies of mass 1 are written. The selection is stable because it is based on identifiers. Each kept particle carries the weight of the particles it stands for, and the renderer brightens it accordingly.

### Version history

| Version | Change |
|---|---|
| 1 | first version |
| 2 | thresholds of ejected clusters in the header (`PhysicsParams`) |
| 3 | one quantization box per subsystem in each snapshot |
| 4 | orbital velocity factor in the header (initial conditions) |

The headers of versions 1 to 3 are in `legacy.rs`. Snapshots of versions 1 and 2 hold a single box. Those of versions 3 and 4 have the same layout. **A resumed recording keeps the version of its file.** `PhysicsParams` and `SimulationConfig` appear in the headers of both formats, so changing them changes both.

### Playback

Between two snapshots, each body follows a cubic Hermite interpolation of its positions and velocities. An absorbed body joins the body that absorbs it. The next snapshots are loaded in the background.

---

## Inspection tools

```powershell
cargo run --release -p stellar-persistence --example inspect -- <file>        # header, versions, snapshots
cargo run --release -p stellar-persistence --example frame_size -- 1000000    # size and cost of a snapshot
```
