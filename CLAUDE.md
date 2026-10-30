# Stellar — instructions for Claude

3D gravitational N-body simulator in Rust (wgpu + egui) for Windows. Development machine: Windows, NVIDIA RTX 4060 Ti (8 GB dedicated), 16 GB of RAM.

Read before any substantial change: `SPEC.md` (rules in force), `README.md` (usage, architecture, tests), and the relevant file in `docs/` (`formats.md`, `gpu-engine.md`, `subsystems.md`; `history.md` for the history of decisions).

Project skills (`.claude/skills/`):
- `new-feature`: checklist for any user-visible change (questions, translations, docs, tests);
- `file-format`: mandatory procedure before touching a saved or recorded structure;
- `perf-measurement`: benchmarks, profiling, reference timings, known performance leads.

## Communication and languages

- Talk to the user in **elaborate French with a rich vocabulary**.
- Do not try to prolong the discussion; ask a question only if it helps answer the request. Do ask every clarifying question **before** implementing a feature.
- **Documentation** (`*.md`, skills) is in **English**.
- **Code comments, error messages of the lower layers, CLI options and help, commit messages** are in **French**.
- The **interface** has French source strings translated into English (`stellar-app/src/i18n.rs`): every displayed text goes through `tr("…")` or `tr!("… {} …", x)` and gets its entry in the `EN` table. Numbers are formatted by language (`format.rs`).
- Update `README.md` and `SPEC.md` with every feature.

## Commands

```powershell
cargo build --release                      # target\release\stellar.exe and stellar-cli.exe
cargo test --workspace                     # GPU tests are skipped without a card
cargo clippy --workspace --all-targets
```

Toolchain: Rust **1.89** at least (`rust-version` in `Cargo.toml`, imposed by locked dependencies: `uuid`, `icu_*`). GPU tests also run without a card with software rendering: `WGPU_BACKEND=gl` (plus `xvfb-run` under Linux).

Quick starts for testing (full table in the README): `STELLAR_AUTOSTART=<n>`, `STELLAR_AUTODISK=1`, `STELLAR_ENGINE=cpu-barnes-hut|gpu-barnes-hut`, `STELLAR_AUTORECORD=<interval>`, `STELLAR_LOAD=<save>`, `STELLAR_REPLAY=<recording>`, `STELLAR_LANG=fr|en`. Console: `stellar-cli.exe --corps 2000 --pas 300`, `--charger`, `--reprendre`, `--help`; unknown argument gives exit code 2.

Two executables share one library (`stellar_app`, `lib.rs`): `stellar.exe` (windowed; with arguments it forwards to stellar-cli) and `stellar-cli.exe` (console; the prompt waits for it). The library is also the composition root: engine registration and `present_mode`. `saves\` and `recordings\` are relative to the current directory.

## Architecture (clean architecture)

The crate table is in the README. Rules:

- The rest of the application knows the engines only through the domain traits. A new engine is added through a factory registered in `stellar-app/src/lib.rs`. `EngineFactory::create` returns a readable error rather than panicking (no card, insufficient memory).
- Dependencies point to the domain: no egui or wgpu in `stellar-domain` / `stellar-application`.
- GPU engine by default if the probe succeeds; fall back to the CPU when loading a save otherwise.

## Delicate points

- **Live view (GPU engine)**: the engine computes on the UI's device and rendering reads its buffers without copy (`PhysicsEngine::live_view`). The full state (`Snapshot::state`, shared as `Arc`) is read back only every one to two seconds: it may **lag** behind `Snapshot::step`. Any data taken from `snapshot.state` must stay consistent with `state.step`, not with `snapshot.step`. Under OpenGL, the engine opens its own device and the display goes back to copies.
- **Presentation and device lock**: in wgpu 24, `Surface::get_current_texture` holds the `device.fence` lock while waiting for the screen, and `queue.submit` needs it. In Fifo mode every GPU-engine submission waited up to a frame (~25 ms per step whatever the size). Hence Mailbox (`present_mode` in `lib.rs`; `STELLAR_PRESENT` forces it). Under DirectX 12, FXC shader compilation takes ~100 s: stay on Vulkan.
- **GPU step**: one main submission, one wait (readback buffer: counters, moments, tracked positions, first pairs), then mergers without waiting. `positions_of` tracks up to 4 identifier lists (identifier → index table on the GPU); a new list is read immediately, then every step. Accessors must not read the device back. Sequence and kernels: `docs/gpu-engine.md`.
- **GPU limits**: `MAX_BODIES = 65,535 × 256`, `MAX_PAIRS = 2¹⁸` contact pairs per step. The renderer's instance buffers are split into chunks (4 M instances at most) to respect `max_buffer_size` (crash at 10 M bodies in the past). GPU bodies are 48 bytes (`GpuBody`, `common.wgsl`: start-of-step position = `pos − vel·dt`). The Barnes-Hut traversal's stack and list are sized from `max_compute_workgroup_storage_size` (prefix of the `forces` module).
- **Precision**: positions are `f64` on the CPU side; rendering works relative to an anchor (`anchor`) to stay accurate in `f32`. The GPU engine is single precision (median acceleration error ~10⁻³ at θ = 0.5).
- **Sub-simulations**: the simulation thread always wraps the engine in `MultiSystem`. `state()` returns bodies in global coordinates grouped by subsystem (`SimulationState::systems`); live view through `SystemsView`. `set_external_field` receives the end-of-step field and the first half kick reuses the previous one. Structure operations (split, merge, recentring) rebuild engines from their state. Details: `docs/subsystems.md`.
- **User files**: the application's repositories delete to the recycle bin (`with_trash`, `stellar-persistence/src/files.rs`); tests delete permanently in a temporary folder. A save's name is its file name (the header keeps the original name). Simulation name: `SimulationHandle::name`, taken from the file on load (`name_from_file`). Files in `saves\` and `recordings\` can weigh tens of gigabytes: never modify them for a test.
- **egui fonts**: some characters are missing (●, ✦, U+202F); check that a symbol is displayed before using it.
- **Camera**: orientation by quaternion (local frame: X forward, −Y right, Z up), rotations in the screen's frame, free roll. Modes `Free`, `Orbit { target, distance }`, `Barycenter { distance }`. Free-flight speed is proportional to the distance to the selection, otherwise to the barycenter.

## File formats

`.stellar` version 6 (5, 4, 3 read) and `.stellarec` version 4 (3, 2, 1 read; a resumed recording keeps its file's version). bincode is not self-describing: **any field change requires a new version**, a copy of the old form in `legacy.rs` and a reading test. **Use the `file-format` skill.** `PhysicsParams` and `SimulationConfig` are also in the recording header. Layout: `docs/formats.md`.

## Verification

1. **Before handing over**: targeted tests only (`cargo test -p <modified crate>`, plus `cargo build --release` if the user is to try the app). No visual check on my side: the user tries it and validates.
2. **After their validation, before committing**: `cargo test --workspace` and `cargo clippy --workspace --all-targets`; fix before committing.

Add unit or integration tests (`crates/*/tests/`) for any new logic, especially file formats and physics.

## Git

- Commit messages in French: a title, then a list of changes.
- Line endings: LF in the repository (`.gitattributes`, `eol=lf`). Files may be CRLF in the Windows working copy; Git normalizes them. Never commit a line-ending conversion mixed with a real change.
- Commit or push only when asked.
- `*.exe`, `*.stellar`, `*.stellarec` are ignored.
