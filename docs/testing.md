# Tests and benchmarks

```powershell
cargo test --workspace                     # unit and integration tests (GPU tests are skipped without a card)
cargo clippy --workspace --all-targets
cargo bench -p stellar-engine-cpu          # time per step for 10k / 100k / 1M bodies (criterion)
```

GPU tests also run without a graphics card, with software rendering: `$env:WGPU_BACKEND="gl"`.

The pre-push hook (`.githooks/pre-push`) runs the tests without the GPU crate and clippy. On GitHub, `Verify` runs them on every pull request, GPU tests included under `xvfb-run` with `WGPU_BACKEND=gl`; a pull request without risk then merges by itself, the others get `to test`, `to acknowledge` or `to review` (rules: [dev-kit's auto-merge](https://github.com/Jyhess/dev-kit/blob/main/docs/auto-merge.md)). Both also run `.github/scripts/architecture.py`, which checks the dependencies between crates and the external dependencies of `stellar-domain` and `stellar-application` against [`architecture.puml`](architecture.puml).

## Measurement tools

| Command | Measures |
|---|---|
| `cargo run --release -p stellar-engine-gpu --example bench -- 1000000 10` | CPU / GPU benchmark: body count, step count; `gpu` or `cpu` to test a single engine, `disc` for a rotating disc. `STELLAR_GPU_PROFILE=1`: time per kernel. |
| `cargo run --release -p stellar-engine-cpu --example timing -- 1000000 5` | CPU engine with its evolution (remaining bodies, mergers, energy drift) |
| `cargo run --release -p stellar-application --example interp_timing -- 1000000` | cost of interpolating one playback frame |
| `cargo run --release -p stellar-persistence --example frame_size -- 1000000` | size and cost of a recording snapshot |
| `cargo run --release -p stellar-persistence --example inspect -- <file>` | contents of a save or a recording |

## Environment variables

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

## What the tests cover

- **physics** (`stellar-engine-cpu/tests/physics.rs`, with the same cases on the GPU in `stellar-engine-gpu/tests/gpu.rs`): Barnes-Hut accuracy against the direct sum, energy conservation on a two-body orbit, conservation of mass and momentum through mergers, fast crossings, chained mergers, cleanup and ejected clusters;
- **GPU engine** (`gpu.rs`): agreement with the CPU engine over several steps, tracked positions, live view, external field and insertion;
- **ejected clusters** (`stellar-domain/tests/ejection.rs`): friends of friends, pruning of unbound members, thresholds, spherical approximation for large clusters, local cluster of a body;
- **sub-simulations** (`stellar-application/tests/systems.rs`): split, merge, escapees, removal of negligible lone bodies, agreement with the full computation;
- **files** (`stellar-persistence`): reading older versions (saves 3 to 5, recordings 1, in the internal tests of `lib.rs` and `recording.rs`), continuing a recording, renaming and deleting linked files, simulation without display (`tests/continuation.rs`, `tests/files.rs`);
- **interface** (`stellar-app`): command-line parsing, the translation table (duplicates, placeholders, every interface text translated into French) and the translation of nested errors (`errors.rs`).
