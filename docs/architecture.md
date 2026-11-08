# Architecture

The project follows **clean architecture**: every dependency points towards the domain, and `stellar-app` is the composition root. The components and their allowed dependencies are drawn in [`architecture.puml`](architecture.puml), checked by `.github/scripts/architecture.py` with the verification (`--write` rewrites the crates and arrows from the code).

| Crate | Role |
|---|---|
| `stellar-domain` | Bodies (`BodySet`, a structure of arrays), state, parameters, physical rules (radius, merger, swept distance, cleanup of ejected bodies and cluster detection in `ejection.rs`, field between subsystems in `systems.rs`), mass ranking, **`PhysicsEngine` / `EngineFactory` traits**. No infrastructure. |
| `stellar-application` | Initial conditions, decoupled simulation thread (`SimulationHandle`, snapshots published at up to 60 Hz), composite engine of sub-simulations (`MultiSystem`, `systems.rs`), engine registry, recording (`Recorder`) and playback, ports, `SimulatorService` facade. |
| `stellar-engine-cpu` | CPU engine, freely optimized. See [`cpu-engine.md`](cpu-engine.md). |
| `stellar-engine-gpu` | GPU engine (WGSL compute shaders): radix sort, Karras LBVH, grouped Barnes-Hut traversal, contacts on the GPU, merger union-find on the CPU. Exposes its buffers (`BodiesView`) for the live view. See [`gpu-engine.md`](gpu-engine.md). |
| `stellar-persistence` | `.stellar` and `.stellarec` files, older versions (`legacy.rs`), deletion to the recycle bin (`files.rs`). See [`formats.md`](formats.md). |
| `stellar-app` | egui screens (`home.rs`, `sim.rs`), camera (`camera.rs`), picking (`picking.rs`), body rendering (`render/`: additive billboards, positions relative to an anchor to keep `f32` precision), translations (`i18n.rs`), command line (`console.rs`). Composition root (`lib.rs`), with two executables: `stellar.exe` (`main.rs`) and `stellar-cli.exe`. |

## Rules

- The rest of the application knows the engines only through the domain traits (`PhysicsEngine`, `EngineFactory`). `EngineFactory::create` returns an `EngineError` rather than panicking (no card, insufficient memory).
- No egui or wgpu in `stellar-domain` / `stellar-application`.
- Inside the engines, code is freely optimized (structure of arrays, SIMD, GPU).
- GPU engine by default if the probe succeeds; a save falls back to the CPU engine on load otherwise.

## Executables

Two executables share one library (`stellar_app`, `lib.rs`): `stellar.exe` (windowed; with arguments it forwards to stellar-cli) and `stellar-cli.exe` (console; the prompt waits for it). The library is also the composition root: engine registration and `present_mode`. `saves\` and `recordings\` are relative to the current directory.

## Rendering

- Positions are `f64` on the CPU side; rendering works relative to an anchor (`anchor`) to stay accurate in `f32`.
- The renderer's instance buffers are split into chunks of at most 4 M instances, to respect `max_buffer_size` (a single buffer fails at 10 M bodies).
- Camera: orientation by quaternion (local frame: X forward, −Y right, Z up), rotations in the screen's frame, free roll. Modes `Free`, `Orbit { target, distance }`, `Barycenter { distance }`. Free-flight speed is proportional to the distance to the selection, otherwise to the barycenter.

## Adding a computation engine

1. Create a crate that implements `stellar_domain::PhysicsEngine` and `stellar_domain::EngineFactory`, with a unique `id`. `EngineFactory::create` must return an `EngineError` rather than panic (no card, insufficient memory); a new variant gets its translation in `stellar-app/src/errors.rs`.
2. Register it in `crates/stellar-app/src/lib.rs`:

```rust
registry.register(Arc::new(MyEngineFactory));
```

The engine then appears in "Advanced parameters → Computation engine" and can be forced with `STELLAR_ENGINE=<id>`. Its identifier is stored in saves. For the command line, add its short name to the `--engine` option in `crates/stellar-app/src/console.rs`, which maps `gpu` and `cpu` to identifiers.
