# Code map

Where things live, to find the place to change without searching. Symbols are named rather than line numbers (they drift): jump to them with rust-analyzer (workspace symbols, go to definition, find references) or `grep -n "fn name"`.

Kept up to date with the `code-map` skill. The test `code_map_lists_every_source_file` (`crates/stellar-app/tests/code_map.rs`) fails if a source file under `crates/*/src` is missing from this map.

Layers and crate roles: README "Architecture". Dependencies point to the domain; `stellar-app` is the composition root.

## Where to change… (common paths)

| Change | Path through the code |
|---|---|
| **Creation parameter** (cloud, masses, velocities) | `InitialConditions` (`domain/config.rs`) → `generate_bodies` and helpers (`application/generator.rs`) → form: `HomeScreen::new_simulation_form` / `advanced` / `initial_conditions` (`app/home.rs`) → CLI: `Options`, `parse`, `usage` (`app/console.rs`) → serialized: **`file-format` skill** (`persistence/legacy.rs`, `lib.rs` `VERSION`) |
| **Physics parameter** (θ, ε, dt, radius, cleanup) | `PhysicsParams` (`domain/config.rs`) → CPU: `CpuEngine` (`engine-cpu/lib.rs`, `octree.rs`) → GPU: `Params` + `GpuEngine::write_params` (`engine-gpu/lib.rs`) **and** `struct Params` in `shaders/common.wgsl` (same layout) → form `HomeScreen::advanced` → serialized: `file-format` skill (also in the recording header) |
| **Keyboard shortcut** | `SimScreen::handle_shortcuts` (`app/sim.rs`); mouse / flight keys: `SimScreen::handle_navigation`; key help: `SimScreen::menu` (list of `(key, description)`) and the hint line of `SimScreen::paint_overlay`; README "Controls" |
| **HUD / side panels** | `SimScreen::hud` (entry), `clusters_panel`, `traces_panel`, `live_panel`, `view_settings`, `recording_panel`, `replay_panel` (`app/sim.rs`) |
| **Data shown live** | add a field to `Snapshot`, fill it in `Worker::publish` (`application/runner.rs`); read it through `Source::latest` (`app/source.rs`). Data from `snapshot.state` must stay consistent with `state.step` |
| **Order to the simulation thread** | `Command` enum + `SimulationHandle` method + handling in `Worker::run_loop` (`application/runner.rs`); forwarded by `Source` (`app/source.rs`) if playback must answer too |
| **Use case on files** (save, load, rename, link) | `SimulatorService` (`application/service.rs`) → ports `SimulationRepository` (`application/ports.rs`), `RecordingRepository` (`application/recording.rs`) → `FileRepository` (`persistence/lib.rs`), `FileRecordingRepository` (`persistence/recording.rs`), `files.rs` |
| **Saved or recorded field** | **`file-format` skill** first: `SaveData` / `ViewState` (`ports.rs`), `RecordingHeader` / `Keyframe` (`application/recording.rs`), `encode_frame` / `decode_frame` (`persistence/recording.rs`) |
| **Camera behaviour** | `Camera`, `Mode` (`app/camera.rs`); driven by `SimScreen::handle_navigation`, `toggle_follow`, `free_speed`, `start_leveling` (`app/sim.rs`); saved as `ViewState` via `Camera::to_view` / `from_view` |
| **Body appearance** | `mass_color`, `Instance`, `Uniforms`, `PointsCallback` (`app/render/mod.rs`) + `render/points.wgsl` (uniform layout shared); batch building: `SimScreen::prepare_batch`, `paint_scene` |
| **Overlay drawings** (markers, triad, grid) | `SimScreen::paint_overlay`, `paint_edge_marker`, `paint_triad`, `paint_disk_grid` (`app/sim.rs`) |
| **Selection** | `pick`, `visible_top_k` (`app/picking.rs`); `SimScreen::select_index`, `select_heaviest`, `select_neighbour`; ranking `domain/ranking.rs` |
| **Traces** | `TraceSet`, `Trace`, `TraceReference`, `ClusterTrack` (`application/trace.rs`); recorded in `Worker::step`; UI `SimScreen::toggle_traces`, `traces_panel`, `TraceFrame` |
| **Ejection / clusters rule** | `sort_out`, `friends_of_friends`, `EjectionRules`, `local_cluster` (`domain/ejection.rs`); called by `CpuEngine::cleanup` and `GpuEngine::sort_out_far` |
| **Sub-simulations** | rules and field: `domain/systems.rs`; engine: `MultiSystem` (`application/systems.rs`: `step`, `try_split`, `try_merge`, `recenter`, `prune_negligible`); docs `subsystems.md` |
| **Merger rule** | `merge`, `body_radius`, `swept_min_distance` (`domain/physics.rs`); CPU `find_contacts` (`engine-cpu/collisions.rs`), `CpuEngine::resolve_collisions`; GPU `collide.wgsl`, `GpuEngine::plan_merges`, `apply_merges.wgsl` |
| **New method on engines** | `PhysicsEngine` (`domain/engine.rs`) → `CpuEngine`, `GpuEngine`, **and** `MultiSystem` (which forwards to its subsystems) |
| **New engine** | README "Adding a computation engine": `EngineFactory` + registration in `app/lib.rs` (`run_gui`) and `--engine` mapping in `console.rs` |
| **Translated text** | `tr` / `tr!` at the call site, entry in `EN` of `app/i18n.rs`; numbers `app/format.rs` |
| **New error case** | variant of the lower layer's enum (`EngineError` `domain/engine.rs`, `RepositoryError` / `FormatError` `application/ports.rs`, `AppError` / `ContinuationError` `application/service.rs`) with its English `Display` → its `Localized` arm (`app/errors.rs`, does not compile without it) and its `EN` entry |
| **Environment variable** | read in `StellarApp::autostart` (`app/app.rs`; `STELLAR_LOAD`, `REPLAY`, `AUTOSTART`, `ENGINE`, `AUTODISK`, `AUTORECORD`), `i18n::init` (`STELLAR_LANG`), `present_mode` (`app/lib.rs`, `STELLAR_PRESENT`), `SimScreen::new` (`STELLAR_STATS`), `Gpu::open` (`engine-gpu/gpu.rs`, `STELLAR_GPU_PROFILE`); README "Environment variables" |
| **Screen flow** (home → simulation → background) | `Screen`, `StellarApp::update` (`app/app.rs`); actions `HomeAction`, `SimAction`, `HeadlessAction` |
| **Run without display** | `HeadlessRun`, `RunTarget` (`application/headless.rs`); UI `HeadlessScreen` (`app/headless.rs`); CLI `console::run` |
| **Playback** | `ReplayPlayer`, `PlaybackClock`, `interpolate` (`application/replay.rs`); `Replay::tick` (`app/source.rs`); UI `SimScreen::replay_panel` |
| **Recording while running** | `Recorder` (`application/recording.rs`); `Worker::start_recording` / `record_keyframe` / `suspend_recording` (`runner.rs`); `SimulatorService::start_recording`, `continuable_recordings` |

## stellar-domain

| File | Contents |
|---|---|
| `lib.rs` | Module list, `Vec3 = DVec3` |
| `body.rs` | `BodyId`, `BodySet` (structure of arrays: ids, positions, velocities, masses) |
| `config.rs` | `InitialConditions` (`Distribution`, `DensityProfile`, `VelocityMode`, `DustMasses`, `Planets`), `PhysicsParams`, `SimulationConfig`, limits (`MAX_PLANETS`, `MAX_DUST_MASS`…) |
| `engine.rs` | **`PhysicsEngine`**, **`EngineFactory`**, `EngineError`, `EngineDescriptor`, `StepReport`, `Diagnostics` |
| `physics.rs` | `body_radius`, `swept_min_distance`, `merge` / `MergeInput` |
| `ejection.rs` | Cleanup of ejected bodies: `Candidate`, `EjectionRules`, `sort_out`, `friends_of_friends`, `EjectedCluster`, `local_cluster` |
| `ranking.rs` | Mass order: `rank_cmp`, `heaviest`, `neighbour`, `rank_of`, `top_k` |
| `state.rs` | `SimulationState` (step, time, bodies, systems) |
| `systems.rs` | Subsystem rules: `SPLIT_RATIO`, `MERGE_RATIO`, `negligible_escapee`, `ExternalField` (tidal field), `Moments`, `SystemSpan`, `SystemSummary`, `SystemsView` / `ViewPart` |

Tests: `tests/ejection.rs` (cleanup rule).

## stellar-application

| File | Contents |
|---|---|
| `lib.rs` | Module list |
| `ports.rs` | `SimulationRepository`, `SaveData`, `SaveSummary`, `ViewState`, `RepositoryError`, `FormatError` |
| `registry.rs` | `EngineRegistry` |
| `generator.rs` | `generate_state`, `generate_bodies` (positions `sample_position`, masses `DustMassDraw` / `massive_masses`, velocities `assign_velocities`), `initial_total_mass` |
| `runner.rs` | Simulation thread. `SimulationHandle` (public API: pause, step, traces, recording, `live_view`, `latest`), `Snapshot`, `Phase`, `Command`; `Worker` (`run_loop`, `step`, `apply_fates`, `refresh_full_state`, `publish`, recording methods) |
| `systems.rs` | `MultiSystem` (composite `PhysicsEngine`): `step`, `apply_fields`, `try_split`, `try_merge`, `recenter`, `prune_negligible`, `locate`, `live_view` |
| `service.rs` | `SimulatorService` facade: create / load / save / list / rename / delete, `start_loaded`, recordings and their links (`continuation_link`, `continuable_recordings`), `open_replay`; `name_from_file`, `AppError`, `ContinuationError` |
| `recording.rs` | `Keyframe`, `BodyFate`, `apply_dust_budget`, `RecordingHeader`, `RecordingLink`, ports `RecordingWriter` / `RecordingReader` / `RecordingRepository`, `Recorder` (writer thread) |
| `replay.rs` | `ReplayPlayer` (cache, prefetch `Loader`), `PlaybackClock`, Hermite `interpolate`, `build_targets` |
| `trace.rs` | `TraceSet`, `Trace`, `TraceReference`, `ClusterTrack`, `cluster_members`, `relative_to` |
| `headless.rs` | `HeadlessRun`, `RunTarget`, `RecordSettings`, `Progress`, `RunOutcome` |

Tests: `tests/systems.rs` (coupled subsystems). Example: `examples/interp_timing.rs`.

## stellar-engine-cpu

| File | Contents |
|---|---|
| `lib.rs` | `CpuEngineFactory`, `CpuEngine`: `step_inner` (KDK sequence), `compute_forces`, `kick`, `resolve_collisions`, `cleanup`, `update_diagnostics`; `PhysicsEngine` impl |
| `octree.rs` | Pre-order Barnes-Hut tree: `build`, `groups`, `Interactions`, `field_of_group` (grouped traversal), `evaluate` (AVX2) |
| `morton.rs` | 63-bit Morton codes: `encode`, `Cube` |
| `collisions.rs` | Swept contacts: hierarchical `Grid`, `find_contacts`, `DisjointSets` |

Tests: `tests/physics.rs`. Bench: `benches/step.rs`. Example: `examples/timing.rs`.

## stellar-engine-gpu

Step sequence and kernels: `docs/gpu-engine.md`.

| File | Contents |
|---|---|
| `lib.rs` | Constants (`MAX_BODIES`, `MAX_PAIRS`, `REBUILD_EVERY`…), `GpuBody`, `Params` / `GpuField` (mirror `common.wgsl`), `readback` offsets, `GpuEngineFactory` (`probe`, `attach`), `Kernels`, `Buffers`, `BodiesView` (live view), `GpuEngine`: `build` (allocation), `step_inner`, `encode_*` (sort, tree and forces, moments, queries, follow, compaction, far), `plan_merges`, `sort_out_far`, `refresh_queries`, `read_bodies`; `PhysicsEngine` impl |
| `gpu.rs` | wgpu wrapper `Gpu`: `open` / `shared`, `kernel` (prepends `common.wgsl`), buffers (`storage`, `readback`, `uniform`), `run*`, `encoder`, `submit`, `read*`, `Profiler` |
| `shaders/common.wgsl` | `GpuBody`, `Params`, `Field` layouts shared by every kernel |

Kernels (`shaders/<name>.wgsl`):

| Kernel | Role |
|---|---|
| `bbox_partial`, `bbox_final`, `morton`, `radix_hist`, `radix_scatter`, `scan_blocks`, `scan_add` | Bounding box, Morton codes, radix sort, prefix sums |
| `lbvh_build`, `lbvh_depth`, `lbvh_levels`, `lbvh_list`, `lbvh_aggregate` | Karras tree: build, depth, levels, list, aggregate (moments of nodes) |
| `forces` | Grouped Barnes-Hut traversal (stack and list sized by the engine's module prefix) |
| `kick_drift`, `kick_moments` | Integration and global moments |
| `collide`, `apply_merges`, `alive_flags`, `compact`, `gather`, `cleanup` | Contacts, mergers, compaction, far bodies |
| `index_map`, `query` | Identifier → index table, tracked positions (`positions_of`) |
| `moments_partial`, `moments_final` | Moments for subsystems |

Tests: `tests/gpu.rs` (skipped without a card). Example: `examples/bench.rs`.

## stellar-persistence

| File | Contents |
|---|---|
| `lib.rs` | `.stellar` format (`VERSION`, `OLDEST`, `read_header`, `FileRepository::write` / `load` / `list` / `relink`) |
| `recording.rs` | `.stellarec` format: `encode_frame` / `decode_frame` (quantization), `FileRecordingWriter`, `FileRecordingReader` (index, `scan` recovery), `FileRecordingRepository` (`create`, `resume`, `list`) |
| `legacy.rs` | Former shapes (`…V3` to `…V6`) and their `From` conversions |
| `files.rs` | `sanitize`, `delete` (recycle bin), `rename` |

Tests: `tests/continuation.rs`, `tests/files.rs`. Examples: `examples/inspect.rs`, `examples/frame_size.rs`.

## stellar-app

| File | Contents |
|---|---|
| `lib.rs` | Composition root: `run_gui` (engine registration, `SAVES_DIR`, `RECORDINGS_DIR`), `wgpu_configuration`, `present_mode`, `install_panic_hook` |
| `main.rs`, `cli.rs` | Entry points of `stellar.exe` and `stellar-cli.exe` |
| `app.rs` | `StellarApp`, `Screen` transitions, `autostart` (environment variables) |
| `home.rs` | `HomeScreen`: `show`, `new_simulation_form`, `advanced`, `massive_bodies_rows`, `initial_conditions`, `saves_panel`, file dialogs (`file_window`, `continue_window`), helpers (`help` badge, `count_picker`, `step_estimate`, `default_name`, `max_bodies` from RAM) |
| `sim.rs` | `SimScreen` (see below), `SimAction`, tuning constants at the top |
| `source.rs` | `Source` (live `SimulationHandle` or `Replay`), `weighted_barycenter`, `angular_momentum` |
| `headless.rs` | `HeadlessScreen` (monitoring without display), `hms` |
| `console.rs` | CLI: `usage`, `Options`, `parse`, `run`, `redirect`; tests of the parser |
| `camera.rs` | `Camera`, `Mode`, projection and ray |
| `picking.rs` | `pick`, `visible_top_k` |
| `render/mod.rs` | `PointRenderer` (chunked instance buffers, follow buffer: `prepare_follow`), `Follow` (tracked body or barycenter), `PointsSource` (direct GPU view or copied instances), `PointsCallback`, `mass_color` |
| `render/points.wgsl` | Billboard shader |
| `i18n.rs` | `Lang`, `tr`, `tr!`, `EN` table, translation tests |
| `errors.rs` | `Localized`: text of each lower-layer error variant in the interface's language |
| `format.rs` | `count`, `real`, `percent`, `decimal_sep` |

### Inside `sim.rs` (`SimScreen`)

| Group | Methods |
|---|---|
| Frame | `show` (order of the calls in a frame), `poll_snapshot` / `SnapshotCache`, `log_stats`, `into_background`, `shutdown` |
| Input | `handle_shortcuts`, `handle_navigation`, `set_captured` / `apply_capture` |
| Selection and follow | `select_index`, `select_heaviest`, `select_neighbour`, `selected_position`, `refresh_selection_cache`, `sync_watch`, `toggle_follow`, `frame_mass` |
| Barycenter and sub-simulations | `chosen_system`, `select_system`, `track_bary_system`, `bary_*`, `aim_at_barycenter`, `view_barycenter_from_axis` |
| Orientation | `refresh_disk_normal`, `reference_up`, `start_leveling`, `speed_reference`, `free_speed` |
| Traces | `toggle_traces`, `sync_trace_reference`, `cycle_trace_frame`, `refresh_trace_masses` |
| Actions | `toggle_pause`, `step_once`, `toggle_recording`, `start_save` / `poll_save` |
| Drawing | `prepare_batch`, `rebase_threshold` (anchor), `paint_scene`, `paint_overlay`, `paint_edge_marker`, `paint_triad`, `paint_disk_grid` |
| Panels | `hud`, `clusters_panel`, `traces_panel`, `live_panel`, `view_settings`, `recording_panel`, `replay_panel`, `menu` |
