# Stellar — History of decisions

Decisions in the order they were made; v1 came from the initial question session. The rules in force are in [`SPEC.md`](../SPEC.md), which is the reference when this history and the current state disagree. Add a line to the current entry, or a new entry, with every feature.

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
- Documentation in English (README, SPEC, `docs/`, CLAUDE.md, skills); `docs/` split out for formats, GPU engine, sub-simulations and this history (moved out of SPEC, which keeps only the rules in force).
- Esc menu help: F5 label translated, F10 (quit) added.
