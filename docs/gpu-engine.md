# GPU engine

`crates/stellar-engine-gpu` runs the same physics as the CPU engine (Barnes-Hut, KDK Leapfrog with a fixed step, swept contacts, mergers, cleanup) as WGSL compute shaders, in **single precision**. The Rust side is in `src/lib.rs` (engine, step encoding) and `src/gpu.rs` (device, kernels, buffers, profiling). The shaders are in `src/shaders/`; `common.wgsl` is prepended to every module.

- **Accuracy:** median relative error of the acceleration of about 10⁻³ compared with the direct sum (θ = 0.5). After 50 steps, positions differ from the CPU engine by less than 10⁻³.
- **Reference timings** (RTX 4060 Ti, sphere at rest): about 14 ms per step for 1 M bodies, of which about 9 ms are spent on forces; about 62 ms for 4 M bodies.
- **Video memory:** about 240 bytes per body, i.e. 2.4 GB for 10 M bodies.

## Constants (`lib.rs`)

| Constant | Value | Meaning |
|---|---|---|
| `MAX_BODIES` | 65 535 × 256 ≈ 16.7 M | 1D dispatches: 65 535 workgroups of 256 threads |
| `MAX_PAIRS` | 2¹⁸ | contact pairs handled per step |
| `REBUILD_EVERY` | 4 | steps between two rebuilds of the tree topology |
| `FORCE_GROUP` | 128 | bodies per group in the Barnes-Hut traversal |
| `LEVELS` | 72 | maximum tree depth (30-bit keys + 32-bit tie-break) |
| `QUERY_CAP` / `QUERY_LISTS` | 1 024 / 4 | tracked identifiers and tracked lists (traces, camera) |
| `PAIR_PREFIX` / `FAR_PREFIX` | 1 024 | pairs and far bodies read back with the step; any more need an extra readback |
| `NODE_BYTES` | 48 | tree node: center of mass and mass, box min and rc², box max |

A body (`GpuBody`, `common.wgsl`) takes **48 bytes**. The start-of-step position needed by swept contacts is not stored: it is computed as `pos − vel·dt`.

## One step

Main submission, then a single wait:

1. `kick_drift`: half kick (including the external field of the previous step), then drift.
2. **Tree**. Every `REBUILD_EVERY` steps, or after a compaction, the topology is rebuilt:
   - `bbox_partial` → `bbox_final`: bounding box;
   - `morton`: 30-bit Morton keys (10 bits per axis);
   - radix sort: 8 passes of 4 bits (`radix_hist`, scan with `scan_blocks` / `scan_add`, `radix_scatter`);
   - `gather`: reorders the bodies (double buffer `bodies[0/1]`);
   - `lbvh_build`: binary radix tree (Karras, 2012), with internal nodes 0..n−2 and the sorted bodies as leaves;
   - `lbvh_depth` → `lbvh_levels` → `lbvh_list`: depth of each node, level offsets, indirect dispatch arguments.

   On every step, `lbvh_aggregate` then runs level by level from the deepest one (indirect dispatches). It computes the center of mass, box and opening radius rc = longest side / θ + |center of mass − geometric center| of each node, plus the box swept by the trajectories during the step.
3. `forces`: grouped traversal (see below), which writes the accelerations. If the tree was rebuilt or the index table is stale, `index_map` rebuilds the identifier → index table. Entries of vanished bodies are not erased; reads check the identifier.
4. `collide`: each body walks down the swept boxes and records the pairs (i < j) whose trajectories come closer than the sum of their radii.
5. `kick_moments` → `moments_final`: second half kick, and global moments (mass, m·x, m·v, kinetic and potential energy) in the same read of the bodies.
6. On cleanup steps only, `cleanup`: indices and records of the bodies beyond k × R from the barycenter.
7. `query`: positions of the tracked lists, through the index table.
8. Copy into the **readback buffer** (`readback` module): counters, moments, tracked positions, first pairs, first far bodies.

Then on the CPU:

- **Cleanup:** the domain rule (`stellar-domain/src/ejection.rs`) sorts the far bodies into unbound bodies to remove and bound clusters to keep.
- **Mergers:** union-find on the pairs (`plan_merges`), with bodies removed in the same step left out.
- **Second submission, without waiting:** `apply_merges` (resulting body replaced, absorbed bodies set to zero mass), then compaction (`alive_flags`, scan, `compact`). The tree is rebuilt on the next step.
- **Tracked positions:** corrected for the mergers and removals of the step. Accessors (`positions_of`, `center_of_mass`, `diagnostics`) never read the device again.

## Grouped Barnes-Hut traversal (`forces.wgsl`)

The 128 consecutive bodies of a workgroup are neighbours in Morton order, and they walk down the tree together. A node is treated as a point when it is far enough from the **box of the whole group**, which is stricter than the per-body criterion. Accepted nodes and the bodies of nearby leaves go into a list in shared memory. Each thread then evaluates that list for its own body, in an unrolled loop. The stack and the list are sized from `max_compute_workgroup_storage_size`, a prefix of the `forces` module.

## Live view

When it is built on the UI device (`GpuEngineFactory::attach`), the engine exposes its body buffers (`BodiesView`: current buffer, count, `STRIDE`, `POSITION_OFFSET`, `MASS_OFFSET`). The renderer reads them with no copy. Compute and rendering share the same queue, so a draw submitted after the read always sees a complete set of bodies; at worst a step that removes bodies shows a few duplicates for one frame. The full state is read back only every one to two seconds. Under OpenGL, the engine opens its own device and the display goes back to copies.

Under wgpu 24, `Surface::get_current_texture` holds the device fence while it waits for the screen, and `queue.submit` needs that fence. In Fifo mode, every submission of the engine therefore waited up to one frame. The UI uses Mailbox where available (`present_mode` in `stellar-app/src/main.rs`; `STELLAR_PRESENT` forces a mode).

## Profiling

```powershell
$env:STELLAR_GPU_PROFILE=1; cargo run --release -p stellar-engine-gpu --example bench -- 1000000 10 gpu
```

This prints the cumulative time and dispatch count per kernel, using pass timestamps. It works only when the engine has its own device, as in the bench and the tests. Kernel names are the shader file names above.
