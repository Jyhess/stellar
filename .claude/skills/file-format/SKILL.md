---
name: file-format
description: Procedure for any change that touches a serialized structure of Stellar's save (.stellar) or recording (.stellarec) formats — SaveData, SaveSummary, SimulationConfig, InitialConditions, PhysicsParams, SimulationState, ViewState, RecordingHeader, RecordingLink, or the snapshot payload. Use before adding, removing or reordering a field of any of these types, or when old saves fail to load.
---

# Changing a file format

bincode is **not self-describing**: changing any field of a serialized type (or of a type nested in it) silently changes the bytes, and old files no longer decode. The user's saves and recordings can weigh tens of gigabytes. Breaking them is not acceptable.

Reference: `docs/formats.md` (layout, version history). Code: `crates/stellar-persistence/src/{lib.rs, recording.rs, legacy.rs}`.

## 1. Find out which formats are affected

| Type changed | `.stellar` | `.stellarec` |
|---|---|---|
| `SaveData`, `SaveSummary`, `ViewState`, `RecordingLink`, `SimulationState`, `SystemSpan` | yes | no |
| `SimulationConfig`, `InitialConditions`, `PhysicsParams`, `VelocityMode`, `Distribution` | yes | **yes** (header) |
| `RecordingHeader` | no | yes |
| snapshot payload (`recording.rs`, encode / decode) | no | yes |

Check nesting with a search (`grep -rn "pub struct\|pub enum" crates/stellar-domain/src crates/stellar-application/src`). Any type reachable from `SaveData` or `RecordingHeader` counts.

## 2. Bump the version

- `.stellar`: `VERSION` in `stellar-persistence/src/lib.rs` (+ the module documentation's version list).
- `.stellarec`: `VERSION` in `recording.rs` (+ its documentation). A resumed recording keeps the version of its file: the writer must still be able to append snapshots in the old layout if the payload changes.

## 3. Keep the old form in `legacy.rs`

1. Copy the **previous** form of every changed type (and of each type that contains it, up to `SaveData` / `RecordingHeader`) into `legacy.rs`, suffixed with the last version that used it (e.g. `SimulationStateV4`, `InitialConditionsV5`).
2. Write the conversion old → new, filling new fields with the value that reproduces the old behaviour (e.g. orbital factor = 1, a single subsystem, suggested cluster thresholds).
3. Dispatch on the version read in the header (`read_header` in `lib.rs`; header reading in `recording.rs`). Keep `OLDEST` unchanged unless the user asks to drop old versions.

## 4. Test

- Add a reading test for the previous version next to the existing ones: `reads_versions_3_to_5` (`lib.rs`) and `reads_version_1` (`recording.rs`). Write a file in the old form with the legacy types, read it with the current code, and compare.
- Run `cargo test -p stellar-persistence`, then `cargo test -p stellar-application` (recording continuation) if the header changed.
- Never test against files in `saves\` or `recordings\`: use a temporary folder.

## 5. Update the documentation

- `docs/formats.md`: version tables and, if needed, the layout.
- `SPEC.md` ("Files") and a line in the feature's entry in `docs/history.md`.
- `CLAUDE.md`: current version numbers in "File formats".
