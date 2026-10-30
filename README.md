# Stellar

A 3D gravitational N-body simulator for Windows. A cloud of particles of mass 1 collapses under gravity, and two bodies that touch merge. The computation runs **on the graphics card** (GPU engine, the default: wgpu compute shaders) or **on the CPU** (multithreaded Barnes-Hut). Rendering goes through wgpu in both cases. The interface is available in French and English.

| Document | Content |
|---|---|
| this README | installation, usage, architecture, tests |
| [`SPEC.md`](SPEC.md) | functional specification: the rules currently in force |
| [`docs/history.md`](docs/history.md) | history of decisions, version by version |
| [`docs/formats.md`](docs/formats.md) | `.stellar` and `.stellarec` file formats, version history |
| [`docs/gpu-engine.md`](docs/gpu-engine.md) | GPU engine: kernels, step sequence, constants, profiling |
| [`docs/subsystems.md`](docs/subsystems.md) | sub-simulations: coupling, order of operations, invariants |
| [`CLAUDE.md`](CLAUDE.md), [`.claude/skills/`](.claude/skills) | instructions for Claude Code |

---

## 1. Installation on Windows

### Rust

1. Download and run **rustup-init.exe** from <https://rustup.rs>.
2. If the installer offers it, accept the **Visual Studio Build Tools** ("Desktop development with C++" workload, with the Windows SDK). Rust needs them for linking.
3. Open a **new** terminal (PowerShell) and check the toolchain (Rust **1.89** or later is required):

```powershell
rustc --version
cargo --version
```

### Build and run

From the `stellar` folder:

```powershell
cargo run --release
```

The first build takes a few minutes while the dependencies are downloaded and compiled; later builds are fast. The executables are in `target\release\`: `stellar.exe` (the interface) and `stellar-cli.exe` (the command line, see "Simulation without display").

> Always build with `--release`: in debug mode, the computation is several tens of times slower.

- Display is capped at 120 frames per second. With NVIDIA and AMD cards (Vulkan) or under DirectX 12, it is synchronized in **Mailbox** mode: no tearing, and waiting for the screen does not block the GPU engine, which shares the card (see [`docs/gpu-engine.md`](docs/gpu-engine.md#live-view)).
- Memory: about 420 bytes of RAM per particle (about 4.2 GB for 10 million) and, with the GPU engine, about 240 bytes of video memory per particle.
- Saves go to the `saves\` subfolder and recordings to `recordings\`, both relative to the current directory.

---

## 2. Usage

### Home screen

- **New simulation**: name, number of particles, cloud radius, random seed, distribution (sphere or disc, with thickness and density profile), initial velocity. The velocity modes are rest, random, imposed circular, orbital circular × factor ± spread (the default) and radial. The factor ranges from 0.5 to 1.5: 1 gives circular orbits, less gives a collapse, more an expansion. Each parameter has a "?" badge that explains it.
  - **Name**: used for the simulation's saves (F5) and recordings. By default it combines the date, the abbreviated particle count, the distribution and the initial velocity (`2026-10-28_1M_sph_orb`), plus the time if that name already exists. It follows the parameters until you type one; the "Auto" button restores this behaviour. A loaded simulation takes the name of its save, without "_reprise" or a number.
  - **Number of particles**: a 1-2-5 stepped slider (1,000, 2,000, 5,000, 10,000…) up to the machine's maximum, plus an exact input field. The maximum is the engine limit, 80 % of main memory, or one billion, whichever is lowest.
  - Below the form: the estimated memory and the **estimated duration of the first step**, based on the benchmark.
- **Advanced parameters**: G, dt, softening ε, radius r₀ of a particle of mass 1, accuracy θ, cleanup period and distance, thresholds of ejected clusters (link length, number of bodies, share of the mass), computation engine. By default, dt, ε, r₀ and the link length are derived from the initial conditions:
  - dt = free-fall time / 500;
  - ε = 0.3 × mean distance between neighbours;
  - r₀ = 1 % of that distance;
  - cluster link length = mean distance between neighbours. A cluster is kept from 10 bodies or 0.1 % of the initial mass.
- **Saved simulations**: the list of the `saves\` folder, with a "Browse…" button to open a file stored elsewhere. A loaded simulation starts paused.
- **Rename, delete** (saves and recordings): deleting asks for confirmation and sends the file to the Windows recycle bin, optionally with the linked file (a save's recording, a recording's saves). Renaming also renames the file. A save and its linked recording are renamed together, and their links are updated.
- **Language**: French or English, from the list at the bottom of the home screen or from the Esc menu. The default is the Windows language, and the choice is stored in `%APPDATA%\Stellar\langue`. Numbers follow the language: decimal comma and space as thousands separator in French, point and comma in English.

### Simulation without display

For long computations: no rendering of the bodies, the whole graphics card for the computation, and automatic recording with an adjustable interval and dust budget.

- **From the home screen**: tick "Without display", then choose a stop condition: number of steps, simulated time, real duration, or manual. A monitoring screen shows progress (step, steps/s, estimated remaining time, recording). "Stop and save" closes the recording and saves the final state under the simulation's name. "Show the simulation" switches to the usual display while computation and recording continue. Closing the window stops cleanly.
- **From the command line**: `stellar-cli.exe` is a console application installed next to `stellar.exe`. The prompt waits for it to finish, and Ctrl+C stops it cleanly. `stellar-cli --help` lists the options (option names and help text are in French). An unknown argument is reported with exit code 2. `stellar.exe` started with arguments forwards them to `stellar-cli`.

  ```powershell
  .\stellar-cli.exe --corps 4000000 --disque --pas 200000 --intervalle 20 --nom galaxie
  .\stellar-cli.exe --charger saves\galaxie.stellar --pas 100000
  .\stellar-cli.exe --reprendre recordings\galaxie.stellarec --duree 120
  ```

  - **New simulation**: `--corps` (bodies), `--rayon` (radius), `--graine` (seed), `--disque` (disc), `--vitesse repos|alea|circ|orb|rad` (velocity mode), `--dispersion` (spread), `--facteur` (multiple of the orbital velocity), `--vmin`/`--vmax`, `--moteur gpu|cpu` (engine).
  - **Resume a save**: `--charger FILE`. The recording linked to the save is continued; otherwise a new recording starts. `--poursuivre FILE` instead extends a compatible recording from the save's step, deleting the later snapshots.
  - **Resume a recording**: `--reprendre FILE` loads the latest save linked to the recording (the resume save, or an F5 save made during the recording) and continues it.
  - **In every case**: `--nom` (name), `--intervalle` (snapshot interval), `--poussiere N|toutes` (dust budget), and a target: `--pas N` (steps) or `--temps T` (simulated time), both counted from the save's step when resuming, or `--duree minutes` (real duration). Without a target, it runs until Ctrl+C. Progress is printed every five seconds. On stop, the recording is closed and the final state is saved.

### Controls

Letter keys are read by character and laid out for an AZERTY keyboard.

| Key | Action |
|---|---|
| Z / S | forward / backward (when tracking: closer to / farther from the target) |
| Q / D | left / right (no effect when tracking) |
| A / E | roll the view around the line of sight |
| H | progressively level the horizon: the top of the screen returns to the disc normal |
| Shift / Ctrl | movement × 5 / × 0.2 |
| + / − | speed multiplier |
| Mouse | look around, in the screen's frame (when tracking: orbit the target) |
| Wheel | forward / backward like Z / S (when tracking: distance to the target; Shift speeds it up) |
| Ctrl + wheel | optical zoom |
| F | track the selection (body or barycenter) / stop tracking |
| G | grid of the disc plane |
| Left click | track the aimed body (crosshair, or pointer when the mouse is released) |
| Home | go to the most massive body |
| PgDn / PgUp | next / previous body by mass |
| B | select the barycenter and look at the disc face-on (along the angular momentum axis), keeping the distance |
| C | aim at the center of gravity (free flight) |
| T | trace mode: trajectories of the 10 most massive bodies visible on screen at that moment / clear |
| R | traces: next reference frame (selected body, simulation barycenter, or barycenter of the selected body's bound cluster) |
| V | overview: move back until 90 % of the mass is visible, along the current line of sight |
| Space | pause / resume |
| P | single step |
| Tab | release / capture the mouse |
| Right click | resume aiming when the mouse is released |
| F5 | save |
| F9 | start / stop recording |
| F10 | quit |
| Esc | menu (resume, save, home, quit, key help) |

**Tracking.** A tracked body (or the barycenter) stays at the center of the screen: the mouse orbits the camera around it, Z / S set the distance and A / E roll the view. If the body merges, tracking moves to the resulting body. F hands back free flight without changing the view. Orientation is unrestricted in every direction (you can go over the top of an object); H brings the horizon back to the disc plane. Speed is proportional to the distance to the selected body, or to the barycenter when nothing is selected: slow near the target, fast far away. Moving from one body to another (click, Home, PgDn/PgUp, ranking) keeps the line of sight, the zoom and the distance to the target.

**On-screen guides.** A trihedron in the lower-left corner shows the X, Y, Z axes and the disc normal N. When the barycenter or the selected body is off screen, an arrow at the screen edge shows its direction and distance.

**Traces.** Each trace has its own colour, repeated in the "Bodies" panel. It gets one point per computation step and continues on the body produced by a merger. Two displays are available:

- the full **history** since activation (decimated beyond 4,096 points);
- a **tail** of the last N steps, from 10 to 5,000, fading into the past.

Traces are always shown in a **reference frame** (R moves to the next one). Each point becomes p(t) − r(t) + r(now), where r is the trajectory of the selected body, of the simulation barycenter, or of the barycenter of the selected body's **local cluster**. The local cluster is the bound group that contains the body, found with the same rule as cleanup and updated every one to two seconds; its 512 most massive members are tracked at every step. The reference therefore stays still on screen and orbits around it become visible. Without a selected body, the barycenter is the reference. The barycenter's trajectory is recorded as soon as traces are activated, so the whole history can be replayed in its frame. A selected body's trajectory comes from its own trace when it is traced, otherwise from a track recorded since its selection; in that case the older part of the traces is not shown. The legend gives the mass of each traced body and its share of the present mass. Traces are not saved.

**Panels.** The "Simulation" panel shows the present mass and the ejected mass, i.e. the mass removed by cleanup, with its share of the initial mass. The ranking of the most massive bodies gives each one's share of the present mass, with an extra line for the selected body when it is not in the top ten.

The "Bodies" panel also lists the **sub-simulations** and the **ejected clusters** kept by cleanup (see [Sub-simulations](#sub-simulations)), with marker body (the most massive one), body count, mass and distance to the barycenter.

- **Clicking a sub-simulation** selects its **barycenter** and centres the camera on it, at a distance that shows it entirely. B shows it face-on, and the panel gives its bodies, mass, velocity and size. If it merges with another one, the camera follows the merged whole. If it merges back into the main simulation and no other sub-simulation remains, the camera returns to the global barycenter.
- **Clicking an ejected cluster** selects its marker body.

The cluster list is updated at each cleanup. Neither list exists during playback.

### Recording and playback

A heavy simulation can be recorded and then replayed smoothly at 60 frames per second, with fully free navigation.

- **Record:**
  - **F9**, or the "Simulation" panel, starts or stops recording at any time;
  - on the home screen, "Record from the start" starts recording with the simulation;
  - a snapshot (positions, velocities, masses, mergers) is written every N steps (adjustable) to `recordings\` (`.stellarec` files);
  - writing happens on a dedicated thread and barely slows the computation down.
- **Dust budget (optional):**
  - at most B particles of mass 1 per snapshot (1 million by default), chosen by identifier: always the same ones, with no flicker;
  - more massive bodies are always all recorded;
  - each kept particle is brightened in proportion to the particles it stands for, and the exclusion stops by itself once the body count falls below the budget;
  - a snapshot weighs about 12 bytes per recorded body, i.e. about 12 MB per million.
- **Replay:**
  - from the "Recordings" list on the home screen;
  - between two snapshots, each body follows a cubic Hermite interpolation of its positions and velocities, and an absorbed body joins the body that absorbs it;
  - controls: Space (play / pause), ← / → (previous / next snapshot), ↑ / ↓ (speed), ⌫ (playback direction), time bar;
  - camera, selection, traces (T, R) and overview work as in a live simulation.
- **Suspend and resume:**
  - quitting the program or returning to the home screen during a recording closes the recording cleanly;
  - the simulation is then saved automatically as "<recording>_reprise" and linked to the recording;
  - reloading that save, or clicking "Resume" in the recordings list, continues the recording in the same file as soon as the computation restarts, with no gap or duplicate;
  - reloading an older resume save truncates the recording at that point: later snapshots are deleted.
- **F5 save during a recording:** the save remembers the running recording. Reloading it continues the recording at the save's step and deletes the later snapshots.
- **Continue a recording from any save:**
  - the "Continue a recording…" button of a save lists the compatible recordings (same initial conditions, at least one snapshot before the save) and the number of snapshots that will be deleted;
  - a snapshot is written immediately at the save's step, then the recording goes on at its original interval;
  - bodies that disappeared between the last kept snapshot and the save get their real fate when the next snapshot of the old recording has it; otherwise they are marked "eliminated";
  - a recording of another simulation is refused, which is detected when the save's massive bodies are missing from the recording.
- **After a crash:** an interrupted recording remains readable, because its index is rebuilt on opening.

### GPU engine

If a compatible graphics card (DirectX 12 or Vulkan) is detected, the **GPU** engine becomes the default engine. The CPU engine remains available in "Advanced parameters → Computation engine", which also shows the detected card.

- **Same algorithm as the CPU engine:** Barnes-Hut, Leapfrog, swept mergers, cleanup, in single precision.
- **Capacity:** at most 16.7 million bodies.
- **Performance** (RTX 4060 Ti, sphere at rest): about 14 ms per step for 1 million bodies, 62 ms for 4 million.
- **Automatic fallback:** a save made with the GPU engine reloads with the CPU engine when no card is available.
- **Live view:** the engine computes on the interface's graphics card, which draws the bodies straight from the engine's buffers, with no copy. The full state is read back only every one to two seconds (picking, ranking, barycenter, saving). The step, energies, center of mass and the tracked body's position are up to date in every snapshot.

Internals, kernels and profiling: [`docs/gpu-engine.md`](docs/gpu-engine.md).

---

## 3. Troubleshooting

| Symptom | Cause and remedy |
|---|---|
| Error dialog at startup or during a run | Details are written to `stellar-crash.log` in the current directory. |
| No GPU engine in "Computation engine" | No compatible card was detected (DirectX 12 or Vulkan). The CPU engine is used, and GPU saves reload with it. |
| Very long first start (around 100 s) | Shader compilation by FXC under DirectX 12. Use Vulkan: `$env:WGPU_BACKEND="vulkan"`. |
| Choppy camera, or about 25 ms per step whatever the size | The display is in Fifo mode, which blocks GPU submissions. Check the mode with `STELLAR_STATS=1` and force `STELLAR_PRESENT=mailbox` if the card supports it. |
| Creation refused with an error message | The engine could not be created (no card, insufficient memory). Reduce the body count; the home screen shows the estimated memory. |
| Display through copies, slower | The interface runs under OpenGL: the engine opens its own device and the display can no longer read its buffers directly. |
| Debug build very slow | Always use `--release`. |

---

## 4. Architecture

The project follows **clean architecture**. Every dependency points towards the domain:

```
stellar-app (presentation + composition root: egui, wgpu)
   │
   ├──► stellar-persistence (adapter: .stellar, .stellarec files)
   ├──► stellar-engine-cpu  (adapter: CPU Barnes-Hut engine)
   ├──► stellar-engine-gpu  (adapter: GPU Barnes-Hut engine, wgpu)
   │
   └──► stellar-application (use cases, ports)
              │
              └──► stellar-domain (entities, rules, PhysicsEngine trait)
```

| Crate | Role |
|---|---|
| `stellar-domain` | Bodies (`BodySet`, a structure of arrays), state, parameters, physical rules (radius, merger, swept distance, cleanup of ejected bodies and cluster detection in `ejection.rs`, field between subsystems in `systems.rs`), mass ranking, **`PhysicsEngine` / `EngineFactory` traits**. No infrastructure. |
| `stellar-application` | Initial conditions, decoupled simulation thread (`SimulationHandle`, snapshots published at up to 60 Hz), composite engine of sub-simulations (`MultiSystem`, `systems.rs`), engine registry, recording (`Recorder`) and playback, ports, `SimulatorService` facade. |
| `stellar-engine-cpu` | CPU engine, freely optimized (see below). |
| `stellar-engine-gpu` | GPU engine (WGSL compute shaders): radix sort, Karras LBVH, grouped Barnes-Hut traversal, contacts on the GPU, merger union-find on the CPU. Exposes its buffers (`BodiesView`) for the live view. See [`docs/gpu-engine.md`](docs/gpu-engine.md). |
| `stellar-persistence` | `.stellar` and `.stellarec` files, older versions (`legacy.rs`), deletion to the recycle bin (`files.rs`). See [`docs/formats.md`](docs/formats.md). |
| `stellar-app` | egui screens (`home.rs`, `sim.rs`), camera (`camera.rs`), picking (`picking.rs`), body rendering (`render/`: additive billboards, positions relative to an anchor to keep `f32` precision), translations (`i18n.rs`), command line (`console.rs`). Composition root (`lib.rs`), with two executables: `stellar.exe` (`main.rs`) and `stellar-cli.exe`. |

### CPU engine

At each step:

1. **Kick ½ dt**: half update of the velocities.
2. **Drift dt**: update of the positions; start positions are kept.
3. **Swept contacts**:
   - hierarchical grid over the bounding boxes of the trajectories; very fast or very large bodies go to the upper levels;
   - neighbouring cells are found without a hash table: the sorted cell keys are walked with one cursor per direction;
   - exact minimum distance between straight trajectories;
   - chained mergers grouped by union-find.
4. **Merger**: mass and momentum are conserved and the position moves to the barycenter. The identifier of the most massive member is kept.
5. **Forces**:
   - bodies reordered by Morton code;
   - octree built in parallel, stored in pre-order and walked without a stack;
   - opening criterion `d > s/θ + δ` and Plummer softening;
   - **grouped traversal** of at most 32 neighbouring bodies: the tree is walked once per group, using the group's box for the criterion, then the interaction list is evaluated for each body. With AVX2 and FMA, four bodies are processed per instruction, and the inverse square root uses a Newton-refined estimate.
6. **Kick ½ dt**.
7. **Cleanup, every N steps**: removal of the bodies that are both **unbound** (positive specific energy relative to the barycenter) **and** beyond k × R, **except ejected clusters**. The rule is shared by both engines (`stellar-domain/src/ejection.rs`):
   - far bodies are grouped by "friends of friends" (link length);
   - each group is pruned: members not bound to the group are removed, most energetic first. Energy is measured in the group's frame, under the group's own potential: exactly up to 2,048 members, with spherical symmetry beyond;
   - a bound group of at least n bodies, or of at least a given share of the initial mass, is kept whole even if it moves away indefinitely. A single very massive body counts as a group.

   The GPU engine finds the far bodies on the card and reads back only those, on cleanup steps only.

The computation uses every core but one, which is left to the interface.

### Sub-simulations

An ejected cluster that has moved far enough away becomes a **sub-simulation**: its own engine (of the same type), its own frame (origin at its barycenter, which keeps precision intact even very far away), its own tree. The subsystems feel each other through a linear tidal field. They split, merge and recentre at cleanup steps. Details: [`docs/subsystems.md`](docs/subsystems.md).

### Adding a computation engine

1. Create a crate that implements `stellar_domain::PhysicsEngine` and `stellar_domain::EngineFactory`, with a unique `id`. `EngineFactory::create` must return a readable error rather than panic (no card, insufficient memory).
2. Register it in `crates/stellar-app/src/lib.rs`:

```rust
registry.register(Arc::new(MyEngineFactory));
```

The engine then appears in "Advanced parameters → Computation engine" and can be forced with `STELLAR_ENGINE=<id>`. Its identifier is stored in saves. For the command line, add its short name to the `--moteur` option in `crates/stellar-app/src/console.rs`, which maps `gpu` and `cpu` to identifiers.

---

## 5. Tests and benchmarks

```powershell
cargo test --workspace                     # unit and integration tests (GPU tests are skipped without a card)
cargo clippy --workspace --all-targets
cargo bench -p stellar-engine-cpu          # time per step for 10k / 100k / 1M bodies (criterion)
```

GPU tests also run without a graphics card, with software rendering: `$env:WGPU_BACKEND="gl"`.

### Measurement tools

| Command | Measures |
|---|---|
| `cargo run --release -p stellar-engine-gpu --example bench -- 1000000 10` | CPU / GPU benchmark: body count, step count; `gpu` or `cpu` to test a single engine, `disque` for a rotating disc. `STELLAR_GPU_PROFILE=1`: time per kernel. |
| `cargo run --release -p stellar-engine-cpu --example timing -- 1000000 5` | CPU engine with its evolution (remaining bodies, mergers, energy drift) |
| `cargo run --release -p stellar-application --example interp_timing -- 1000000` | cost of interpolating one playback frame |
| `cargo run --release -p stellar-persistence --example frame_size -- 1000000` | size and cost of a recording snapshot |
| `cargo run --release -p stellar-persistence --example inspect -- <file>` | contents of a save or a recording |

### Environment variables

To start a simulation directly without going through the home screen: `$env:STELLAR_AUTOSTART=200000; cargo run --release`. Most of these variables are read in `crates/stellar-app/src/app.rs`; the others in `lib.rs`, `sim.rs`, `i18n.rs` and the GPU engine.

| Variable | Effect |
|---|---|
| `STELLAR_AUTOSTART=<n>` | simulation of n bodies at startup |
| `STELLAR_AUTODISK=1` | rotating disc instead of a sphere at rest |
| `STELLAR_ENGINE=<id>` | forced engine (`cpu-barnes-hut`, `gpu-barnes-hut`) |
| `STELLAR_AUTORECORD=<interval>` | immediate recording |
| `STELLAR_LOAD=<save>` / `STELLAR_REPLAY=<recording>` | direct load or playback |
| `STELLAR_STATS=1` | one line per second on standard error: step, steps/s, step duration, frames/s, bodies (start from a shell that redirects `2>` to read it) |
| `STELLAR_PRESENT=fifo\|mailbox\|immediate` | forced presentation mode for the interface |
| `STELLAR_LANG=fr\|en` | forced interface language |
| `STELLAR_GPU_PROFILE=1` | time per GPU kernel (benchmark, engine on its own device) |
| `WGPU_BACKEND=vulkan\|dx12\|gl` | graphics API (`gl`: software rendering for tests) |

### What the tests cover

- **physics** (`stellar-engine-cpu/tests/physics.rs`, with the same cases on the GPU in `stellar-engine-gpu/tests/gpu.rs`): Barnes-Hut accuracy against the direct sum, energy conservation on a two-body orbit, conservation of mass and momentum through mergers, fast crossings, chained mergers, cleanup and ejected clusters;
- **GPU engine** (`gpu.rs`): agreement with the CPU engine over several steps, tracked positions, live view, external field and insertion;
- **ejected clusters** (`stellar-domain/tests/ejection.rs`): friends of friends, pruning of unbound members, thresholds, spherical approximation for large clusters, local cluster of a body;
- **sub-simulations** (`stellar-application/tests/systems.rs`): split, merge, escapees, agreement with the full computation;
- **files** (`stellar-persistence`): reading older versions (saves 3 to 5, recordings 1, in the internal tests of `lib.rs` and `recording.rs`), continuing a recording, renaming and deleting linked files, simulation without display (`tests/continuation.rs`, `tests/files.rs`);
- **interface** (`stellar-app`): command-line parsing, and the translation table (duplicates, placeholders).
