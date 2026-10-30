# Stellar — Specification

This document has two parts. **Current state** gives the rules in force; it is the reference when the code and the history disagree. **History** keeps the decisions in the order they were made (v1 came from the initial question session). User-level details are in [`README.md`](README.md), and technical details are in [`docs/`](docs).

---

# Current state

## Scope
- **3D** N-body simulator; initial particles of mass 1; bodies merge on contact.
- **Rust + wgpu + egui**, Windows only (native executables `stellar.exe` and `stellar-cli.exe`).
- Two engines: **GPU** (wgpu compute shaders, single precision; the default when a compatible card is present) and **CPU** (rayon, AVX2/FMA, double precision).
- Target: **1 million particles and more**. GPU limit: 65,535 × 256 ≈ 16.7 M bodies. Creation limit: engine limit, 80 % of main memory, one billion.
- Interface in French and English; documentation in English; code comments, error messages of the lower layers, command-line options and commit messages in French.

## Architecture
- **Clean architecture** (domain / use cases / adapters / infrastructure), except inside the engines, which are freely optimized (SoA, SIMD, GPU).
- Engines are reached only through the `PhysicsEngine` / `EngineFactory` traits. They are selected in the advanced parameters, `STELLAR_ENGINE` or `--moteur`. `EngineFactory::create` returns a readable error rather than panicking.
- File formats are independent of the engine. The GPU engine is the default when the probe succeeds; a GPU save falls back to the CPU engine on load when no card is available.
- Simulation runs on a **dedicated thread, decoupled** from the display, which reads the latest published snapshot (at most 120 frames/s). With the GPU engine, the display reads the engine's buffers directly, and the full state is read back only every one to two seconds.
- The simulation thread always wraps the engine in a composite of **subsystems** (`MultiSystem`; usually a single one).

## Physics
- Forces: **Barnes-Hut** (CPU octree, GPU LBVH), parameter θ, Plummer softening ε; grouped traversal (32 bodies on the CPU, 128 on the GPU).
- Integrator: **Leapfrog (KDK), fixed dt**.
- Spherical bodies, **r = r₀ · m^(1/3)**.
- Contact: **swept detection** (minimum distance over the step); chained mergers through union-find.
- Merger: masses add up, position at the barycenter, momentum conserved; the resulting body keeps the identifier of the most massive member, so tracking follows it.
- Open space. **Periodic cleanup** (every N steps): removal of bodies that are **unbound (energy > 0) AND beyond k × R** from the barycenter, except members of a significant bound ejected cluster (friends of friends; at least n bodies or a share of the initial mass).
- **Sub-simulations**: split when (cluster size + system size) < 0.3 × distance, merge when (sum of sizes) > 0.6 × distance, recentre when the barycenter moves more than half a size from the origin. Coupling through a linear tidal field from point sources, extrapolated to the end of the step. Escapees of a secondary subsystem go to the main subsystem.

## Defaults
- dt = free-fall time / 500; ε = 0.3 × mean distance between neighbours; r₀ = 1 % of that distance; cluster link length = that distance; a cluster is kept from 10 bodies or 0.1 % of the initial mass; θ = 0.7; cleanup every 100 steps at k = 10 × the initial radius.
- Initial velocity: orbital circular, factor 1 (0.5 to 1.5), rotation around Oz, counter-clockwise.

## Creation of a simulation
- Name (default `date_count_distribution_velocity`, with the time added if the name exists), particle count (1-2-5 slider and exact input), cloud radius, seed.
- Distribution: **sphere** or **disc** (thickness, uniform or exponential density profile).
- Initial velocity, **mutually exclusive modes**: rest / random (isotropic, range) / imposed circular (± range) / orbital circular × factor ± spread / radial towards the center (range).
- Advanced panel: G, dt, ε, θ, r₀, cleanup factor k and period, ejected cluster thresholds, engine.
- Estimated memory and estimated duration of the first step.
- During a run, only **pause / resume / single step / speed multiplier** (a cap on steps per second).

## Interface
- Home screen: new simulation, saves (`saves\`, plus "Browse…"), recordings (`recordings\`), rename and delete (to the recycle bin, linked file optional), language.
- Navigation: mouse captured (FPS style); orientation by quaternion with rotations in the screen's frame and free roll; Z/S/Q/D, wheel, Ctrl + wheel for optical zoom. Full table of controls in the README.
- Selection: crosshair + click when captured, free click otherwise; Home / PgDn / PgUp through the mass ranking; B barycenter; clicking a sub-simulation selects its barycenter.
- Tracking: the target stays at the center; changing target keeps the line of sight, the zoom and the distance (never inside the new body).
- Rendering: glowing billboards, real size (minimum 1 px), colour by mass, additive blending; trihedron, off-screen arrows, disc-plane grid.
- HUD: global statistics, selected body, top 10 (plus the selection), present and ejected mass, sub-simulations and ejected clusters.
- Traces (T, R): 10 most massive visible bodies, history or tail, reference frame of the selected body, the barycenter or the local cluster; not saved.

## Files
- **Save** (F5, `.stellar` version 6): full state, parameters, camera, links to recordings.
- **Recording** (F9, `.stellarec` version 4): snapshot every N steps, quantized and compressed, optional dust budget, index rebuilt after a crash; playback by Hermite interpolation; suspend / resume / continue from a save.
- Every change to a serialized structure creates a new version, and older versions remain readable. Layout and history: [`docs/formats.md`](docs/formats.md).

## Simulation without display
- From the home screen (monitoring screen, switch to the display) or with `stellar-cli.exe`. Automatic recording; stop at a step count, a simulated time, a real duration, or on demand; on stop, the recording is closed and the final state is saved under the simulation's name.
- Command line: new simulation, `--charger`, `--poursuivre`, `--reprendre`; `--help`; exit code 2 on an unknown argument.

---

# History

## v1 — initial specification
- 3D N-body simulator, particles of mass 1, merger on contact; Rust + wgpu + egui, Windows only; computation on the CPU (rayon); target 1 M particles.
- Clean architecture, engine behind `trait PhysicsEngine`, engine-independent save format, simulation thread decoupled from a 60 frames/s display.
- Barnes-Hut (octree), Leapfrog KDK with fixed dt, r = r₀ · m^(1/3), swept contacts (spatial grid), union-find, stable identifiers, periodic cleanup of bodies that are unbound and beyond k × the initial radius.
- Creation: sphere or disc, exclusive velocity modes (rest by default), advanced panel.
- Interface: list of saves, mouse permanently captured, Z/S/Q/D, crosshair + click, orbit + tracking, Home / PgDn / PgUp, billboards, HUD, manual save (F5, bincode + zstd).
- Quality: physics tests, criterion benchmarks, README, Rust installation guide.
- Additional decisions: Shift / Ctrl ×5 / ×0.2; + / − movement speed; A / E for up / down (later replaced by roll); wheel moves forward / backward like Z / S (a notch ≈ a quarter of a second of key press; distance to the target when tracking), Ctrl + wheel for optical zoom, mouse sensitivity following the zoom; initial rotation around Oz, counter-clockwise; Space, P; speed multiplier as a step-rate cap; Esc menu; Tab toggles capture; a loaded simulation starts paused.

## v1.1
- Changing target keeps the line of sight, the zoom and the distance; right click resumes aiming; C aims at the center of gravity; V overview (90 % of the mass).
- Display capped at 120 frames/s.
- Trace mode (T) and relative reference frame (R).
- Recording (F9) with snapshots every N steps (21-bit positions, 16-bit velocities), dust budget, F5 saves that continue a recording, continuation from any compatible save.
- Playback: Hermite interpolation, background preloading; no resuming the computation from a recording.
- Suspend / resume of a recording on exit; save formats v2 then v3 (camera roll, barycenter tracking, recording continuation).
- Present and ejected mass; B selects the barycenter seen face-on to the disc.
- GPU engine (Barnes-Hut on LBVH, single precision), the default when a card is present, with fallback to the CPU on load; CPU/GPU benchmark.
- GPU live view: buffers drawn without copy, full state read every one to two seconds; own device and copies under OpenGL.

## v1.2 — performance
- GPU: grouped traversal (128 bodies), topology rebuilt every 4 steps, one submission and one wait per step, mergers after the second half kick, 48-byte bodies, per-kernel profiling, rotating-disc benchmark.
- CPU: grouped traversal (32 bodies), AVX2/FMA, contacts without a hash table.
- Interface: Mailbox presentation where it is guaranteed, because Fifo blocked every GPU submission for a whole frame; `STELLAR_STATS=1`.
- Formats: reading of `.stellar` versions 1 and 2 removed. Version 3, then current, remains readable since.

## v1.3 — ejected clusters
- Cleanup keeps bound groups of far bodies (friends of friends, pruning of unbound members, at least n bodies or a share of the mass); the rule is in the domain and shared by both engines.
- "Bodies" panel: list of kept clusters. Advanced parameters: link length, minimum count and mass share.
- Formats: `.stellar` v4, `.stellarec` v2.

## v1.4 — sub-simulations
- A distant ejected cluster becomes a sub-simulation with its own engine and frame; linear tidal coupling; merge and recentring; escapees go to the main system.
- Formats: `.stellar` v5, `.stellarec` v3 (a resumed recording keeps its version).

## v1.5 — ergonomics
- Simulation name, 1-2-5 slider, orbital velocity by default, "?" badges, estimated first-step duration.
- Rename and delete saves and recordings (recycle bin, linked files).
- Traces: the absolute reference frame is removed (selected body, barycenter, local cluster).
- Mass ranking: extra line for the selected body.

## v1.6 — language and computation without display
- Interface in French or English (Windows language by default, choice remembered, numbers formatted by language).
- Simulation without display from the home screen or the console (first `stellar.exe --sans-affichage`, replaced by `stellar-cli.exe` in v1.7; the option is still accepted and ignored).

## v1.7 — command line
- `stellar-cli.exe`, separate from `stellar.exe` (which forwards arguments to it); `--help`; exit code 2 on an unknown argument.
- Resume without display from a save (`--charger`, `--poursuivre`) or a recording (`--reprendre`); `--pas` and `--temps` counted from the save's step.

## v1.8 — sub-simulations and orbital velocity
- Clicking a sub-simulation selects its barycenter (orbit at three times its size; own panel; B along its own angular momentum). After a merge, the selection moves to the nearest sub-simulation, or to the global barycenter.
- Orbital velocity factor (0.5 to 1.5, `--facteur`, 1 by default), applied before the spread.
- Formats: `.stellar` v6, `.stellarec` v4.

## v1.9 — documentation
- Documentation in English (README, SPEC, `docs/`, CLAUDE.md, skills); `docs/` split out for formats, GPU engine and sub-simulations.
- Esc menu help: F5 label translated, F10 (quit) added.
