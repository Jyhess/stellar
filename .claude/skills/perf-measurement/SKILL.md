---
name: perf-measurement
description: How to measure and profile Stellar's performance — CPU/GPU benchmark, per-kernel GPU profiling, live statistics of the running app, presentation modes, reference timings and the list of known performance leads. Use when asked to speed something up, to compare before/after timings, or to investigate a slowdown (choppy camera, slow steps, long mergers).
---

# Measuring performance

Always measure in `--release`, before and after the change, on the same scene and step count. Report the numbers to the user. Do not report an impression.

## Tools

| Need | Command |
|---|---|
| CPU vs GPU time per step | `cargo run --release -p stellar-engine-gpu --example bench -- <bodies> <steps> [gpu\|cpu] [disque]` |
| Time per GPU kernel | `$env:STELLAR_GPU_PROFILE=1` before the bench (engine on its own device only) |
| CPU engine with its evolution | `cargo run --release -p stellar-engine-cpu --example timing -- 1000000 5` |
| CPU step, statistically | `cargo bench -p stellar-engine-cpu` (criterion, 10k / 100k / 1M) |
| Playback interpolation cost | `cargo run --release -p stellar-application --example interp_timing -- 1000000` |
| Recording snapshot size / cost | `cargo run --release -p stellar-persistence --example frame_size -- 1000000` |
| Running app: steps/s, step time, frames/s | `STELLAR_STATS=1`, one line per second on **stderr** |

Reading `STELLAR_STATS` from the app: `stellar.exe` is a windowed application, so its stderr is visible only through a redirect. Use `$env:STELLAR_STATS=1; $env:STELLAR_AUTOSTART=1000000; .\target\release\stellar.exe 2> stats.txt`, or `STELLAR_AUTODISK=1` for a rotating disc. `stellar-cli.exe` prints its own progress every five seconds.

Presentation mode: `STELLAR_PRESENT=fifo|mailbox|immediate`. Under wgpu 24, Fifo makes every GPU-engine submission wait up to one frame (the device fence is held while waiting for the screen), giving ~25 ms per step regardless of size. Rule it out before blaming the engine.

## Reference timings (RTX 4060 Ti, 16 GB RAM, sphere at rest)

- GPU, 1 M bodies: ~14 ms per step, of which ~9 ms are forces; radix sort ~1.5 ms.
- GPU, 4 M bodies: ~60 ms per step, in one long submission. UI frames wait behind it.
- GPU, massive collapse (hundreds of thousands of mergers in a few steps): up to ~150 ms per step for a few steps.
- GPU sub-simulation: ~0.6 ms per step per small subsystem (own submission and wait).
- Under DirectX 12, FXC shader compilation takes ~100 s: stay on Vulkan.

## Known leads (not done yet)

- Radix sort: wider digits than 8 passes of 4 bits.
- Split the step into several submissions so the camera stays smooth at very large counts.
- Massive collapse: contact pairs are quadratic in the dense core (capped at 2¹⁸ per step); merger planning on the CPU and pair readback.
- Encode the next step while the current one runs (~0.3 to 0.5 ms per step, noticeable at small counts).
- Sub-simulations: compute those below a few thousand bodies on the CPU, or batch their submissions. Escapees are inserted with no acceleration of their own for their first half kick.

When a lead is done or a new one appears, update this list and the reference timings.

Details on the GPU step sequence and kernels: `docs/gpu-engine.md`.
