//! # stellar-engine-gpu
//!
//! Computation engine on the graphics card (wgpu compute shaders: Vulkan,
//! DirectX 12), with the same algorithm as the CPU engine:
//!
//! - **Barnes-Hut on an LBVH**: bounding box, 30-bit Morton codes, GPU radix
//!   sort and parallel construction of the binary radix tree (Karras 2012),
//!   redone every `REBUILD_EVERY` (4) steps; at every step, level-by-level
//!   aggregation (centers of mass, boxes, opening radii, swept boxes), then
//!   a traversal **by groups** of 128 neighbouring bodies, which share the
//!   descent of the tree and an interaction list in shared memory;
//! - **KDK Leapfrog** with a fixed step;
//! - **swept mergers**: pairs in contact are detected on the GPU (same tree,
//!   swept boxes); grouping (union-find, rarely more than a few thousand
//!   pairs) is done on the CPU, then applied on the GPU;
//! - **cleanup** of ejected bodies: distant bodies are found on the GPU and
//!   read back; the domain rule ([`stellar_domain::ejection`]) picks those to
//!   remove while preserving bound clusters; removal and compaction happen
//!   on the GPU.
//!
//! A step costs only **one submission and one wait**: everything the
//! application reads next (contact pairs, removed bodies, global moments,
//! tracked positions) is copied at once into a readback buffer. The planned
//! mergers are applied in a second submission, without waiting. Accessors
//! ([`PhysicsEngine::positions_of`], [`PhysicsEngine::center_of_mass`],
//! [`PhysicsEngine::diagnostics`]) then answer without touching the device.
//!
//! Computation is in single precision (f32); the state is read back in
//! double precision when the application asks for it (save, recording…).
//!
//! Built on the interface's device ([`GpuEngineFactory::attach`]), the
//! engine exposes its body buffers ([`BodiesView`]): the display reads them
//! directly, without a round trip through main memory. Details:
//! `docs/gpu-engine.md`.

mod gpu;

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use rayon::prelude::*;
use stellar_domain::ejection::{sort_out, Candidate, EjectionRules};
use stellar_domain::physics::{merge, MergeInput};
use stellar_domain::{
    BodyId, BodySet, Diagnostics, EjectedCluster, EngineDescriptor, ExternalField, Moments as SystemMoments, EngineFactory, PhysicsEngine, PhysicsParams,
    SimulationState, StepReport,
};

use crate::gpu::{grid, groups, Gpu, Kernel};

pub use crate::gpu::{SharedDevice, WG};

pub const ENGINE_ID: &str = "gpu-barnes-hut";

/// Maximum number of tree levels (30-bit keys + 32-bit tie-break).
const LEVELS: usize = 72;
/// Maximum number of contact pairs handled per step.
const MAX_PAIRS: usize = 1 << 18;
/// Size of a tree node (center of mass, box, opening radius).
const NODE_BYTES: usize = 48;
/// The tree topology (Morton order, Karras construction) is rebuilt every
/// `REBUILD_EVERY` steps, or after a compaction; in between, the tree is
/// only refitted (centers of mass, boxes, opening radii), since the bodies
/// have moved little.
const REBUILD_EVERY: u32 = 4;
/// Bodies per group in the Barnes-Hut traversal (`forces.wgsl`).
const FORCE_GROUP: u32 = 128;
/// Maximum number of bodies (1D dispatches: 65,535 workgroups of 256).
pub const MAX_BODIES: usize = 65_535 * WG as usize;
/// Identifiers whose position is read back at every step (traces, tracking).
const QUERY_CAP: usize = 1024;
/// Identifier lists tracked at every step (the most recently requested).
const QUERY_LISTS: usize = 4;
/// Contact pairs and distant bodies (on cleanup steps) read back with every
/// step; beyond that, an additional readback.
const PAIR_PREFIX: usize = 1024;
const FAR_PREFIX: usize = 1024;
/// Initial capacity of the distant-body buffer (grown when needed).
const FAR_INITIAL: usize = 1 << 14;

// ----------------------------------------------------------------------
// Structures shared with the shaders (identical layout)
// ----------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct GpuBody {
    pub pos: [f32; 3],
    pub mass: f32,
    pub vel: [f32; 3],
    pub id: u32,
    pub acc: [f32; 3],
    pub pot: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct Params {
    n: u32,
    num_wg: u32,
    cap: u32,
    n_ops: u32,
    dt: f32,
    half_dt: f32,
    eps2: f32,
    inv_theta: f32,
    r0: f32,
    g: f32,
    limit2: f32,
    pad: f32,
    field_prev: GpuField,
    field_next: GpuField,
}

/// Linear external field (`common.wgsl`); `center[3] = 0`: none.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuField {
    center: [f32; 4],
    acc: [f32; 4],
    tidal: [[f32; 4]; 3],
}

impl GpuField {
    fn of(field: Option<ExternalField>) -> Self {
        let Some(f) = field else {
            return Self::default();
        };
        let row = |r: [f64; 3]| [r[0] as f32, r[1] as f32, r[2] as f32, 0.0];
        let c = f.center;
        let a = f.acceleration;
        Self {
            center: [c.x as f32, c.y as f32, c.z as f32, 1.0],
            acc: [a.x as f32, a.y as f32, a.z as f32, 0.0],
            tidal: [row(f.tidal[0]), row(f.tidal[1]), row(f.tidal[2])],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct Pass {
    n: u32,
    a: u32,
    b: u32,
    c: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct PairRec {
    a: GpuBody,
    b: GpuBody,
    ia: u32,
    ib: u32,
    pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct MergeOp {
    index: u32,
    kind: u32,
    pad: [u32; 2],
    body: GpuBody,
}

const PAIR_BYTES: usize = std::mem::size_of::<PairRec>();
const BODY_BYTES: usize = std::mem::size_of::<GpuBody>();
const OP_BYTES: u64 = std::mem::size_of::<MergeOp>() as u64;

/// Layout of a step's readback buffer (bytes).
mod readback {
    use super::*;
    /// Number of contact pairs, number of distant bodies (u32).
    pub const COUNTERS: u64 = 0;
    /// Global moments: 3 × vec4<f32>.
    pub const MOMENTS: u64 = 16;
    /// Positions of tracked bodies: QUERY_CAP × vec4<f32>.
    pub const QUERY: u64 = 64;
    pub const PAIRS: u64 = QUERY + 16 * QUERY_CAP as u64;
    /// First distant bodies (indices, then records).
    pub const FAR_INDEX: u64 = PAIRS + (PAIR_PREFIX * PAIR_BYTES) as u64;
    pub const FAR: u64 = FAR_INDEX + 4 * FAR_PREFIX as u64;
    pub const SIZE: u64 = FAR + (FAR_PREFIX * BODY_BYTES) as u64;
}

fn v3(a: [f32; 3]) -> DVec3 {
    DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64)
}

fn f3(v: DVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

fn u32_at(raw: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([raw[offset], raw[offset + 1], raw[offset + 2], raw[offset + 3]])
}

// ----------------------------------------------------------------------
// Factory
// ----------------------------------------------------------------------

/// Factory of the GPU engine.
///
/// By default, each engine opens its own device. Once [`attach`](Self::attach)
/// has been called with the interface's device, the engines created share
/// it and expose their bodies for display without copy.
#[derive(Default)]
pub struct GpuEngineFactory {
    shared: OnceLock<SharedDevice>,
}

impl GpuEngineFactory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks that a usable graphics card is present; returns its name.
    pub fn probe() -> Result<String, String> {
        Gpu::open().map(|g| g.adapter_name)
    }

    /// Shares the interface's device with the engines created afterwards (no
    /// effect if a device is already attached).
    pub fn attach(&self, shared: SharedDevice) {
        let _ = self.shared.set(shared);
    }
}

impl EngineFactory for GpuEngineFactory {
    /// Dispatch limit, and maximum size of a body or node buffer on the
    /// interface's device (if known).
    fn max_bodies(&self) -> Option<usize> {
        let by_buffer = self.shared.get().map_or(usize::MAX, |s| {
            let l = s.device.limits();
            let limit = (l.max_storage_buffer_binding_size as u64).min(l.max_buffer_size);
            (limit / std::mem::size_of::<GpuBody>().max(NODE_BYTES) as u64) as usize
        });
        Some(MAX_BODIES.min(by_buffer))
    }

    fn descriptor(&self) -> EngineDescriptor {
        EngineDescriptor {
            id: ENGINE_ID,
            label: "GPU — Barnes-Hut (carte graphique)",
            description: "Arbre de Barnes-Hut construit et parcouru sur la carte graphique (simple précision).",
        }
    }

    fn create(
        &self,
        state: SimulationState,
        params: PhysicsParams,
    ) -> Result<Box<dyn PhysicsEngine>, String> {
        // Interface device unsuitable for computation (OpenGL, for example): the
        // engine opens its own device, without live display.
        let shared = self.shared.get().filter(|s| Gpu::supports(s));
        let engine = match shared {
            Some(shared) => GpuEngine::with_device(state, params, shared)?,
            None => GpuEngine::new(state, params)?,
        };
        Ok(Box::new(engine))
    }
}

// ----------------------------------------------------------------------
// Engine
// ----------------------------------------------------------------------

struct Kernels {
    kick_drift: Kernel,
    kick_moments: Kernel,
    bbox_partial: Kernel,
    bbox_final: Kernel,
    moments_partial: Kernel,
    moments_final: Kernel,
    morton: Kernel,
    radix_hist: Kernel,
    radix_scatter: Kernel,
    scan_blocks: Kernel,
    scan_add: Kernel,
    gather: Kernel,
    alive_flags: Kernel,
    compact: Kernel,
    lbvh_build: Kernel,
    lbvh_depth: Kernel,
    lbvh_levels: Kernel,
    lbvh_list: Kernel,
    lbvh_aggregate: Kernel,
    forces: Kernel,
    collide: Kernel,
    apply_merges: Kernel,
    cleanup: Kernel,
    index_map: Kernel,
    query: Kernel,
}

impl Kernels {
    fn new(g: &Gpu) -> Self {
        macro_rules! k {
            ($name:literal) => {
                g.kernel($name, include_str!(concat!("shaders/", $name, ".wgsl")))
            };
        }
        // Barnes-Hut traversal: interaction list of 512 entries, stack as large
        // as shared memory allows (between 256 and 2,048 nodes, to keep several
        // workgroups active per multiprocessor).
        const LIST: u32 = 512;
        let shared = g.limits.max_compute_workgroup_storage_size;
        let fixed = 16 * (LIST + 4) + 4 * FORCE_GROUP + 64;
        let stack = (shared.saturating_sub(fixed) / 4).clamp(256, 2048);
        let forces = g.kernel(
            "forces",
            &format!(
                "const STACK: u32 = {stack}u;\nconst LIST: u32 = {LIST}u;\nconst LIST_PAD: u32 = {}u;\n{}",
                LIST + 4,
                include_str!("shaders/forces.wgsl")
            ),
        );
        Self {
            kick_drift: k!("kick_drift"),
            kick_moments: k!("kick_moments"),
            bbox_partial: k!("bbox_partial"),
            bbox_final: k!("bbox_final"),
            moments_partial: k!("moments_partial"),
            moments_final: k!("moments_final"),
            morton: k!("morton"),
            radix_hist: k!("radix_hist"),
            radix_scatter: k!("radix_scatter"),
            scan_blocks: k!("scan_blocks"),
            scan_add: k!("scan_add"),
            gather: k!("gather"),
            alive_flags: k!("alive_flags"),
            compact: k!("compact"),
            lbvh_build: k!("lbvh_build"),
            lbvh_depth: k!("lbvh_depth"),
            lbvh_levels: k!("lbvh_levels"),
            lbvh_list: k!("lbvh_list"),
            lbvh_aggregate: k!("lbvh_aggregate"),
            forces,
            collide: k!("collide"),
            apply_merges: k!("apply_merges"),
            cleanup: k!("cleanup"),
            index_map: k!("index_map"),
            query: k!("query"),
        }
    }
}

/// Pass parameters (sort, scan, queries): a small uniform buffer per use,
/// rewritten only when its value changes. Since the buffers stay the same,
/// their bind groups are created once and for all.
///
/// A given slot must be used with a single value per submission (queue
/// writes happen before the whole submission).
struct PassSlots {
    buffers: Vec<wgpu::Buffer>,
    values: RefCell<Vec<Pass>>,
}

impl PassSlots {
    fn new(gpu: &Gpu, count: usize) -> Self {
        Self {
            buffers: (0..count)
                .map(|_| gpu.uniform("pass", bytemuck::bytes_of(&Pass::default())))
                .collect(),
            values: RefCell::new(vec![Pass::default(); count]),
        }
    }

    fn get(&self, gpu: &Gpu, slot: usize, value: Pass) -> &wgpu::Buffer {
        let mut values = self.values.borrow_mut();
        if values[slot] != value {
            gpu.queue
                .write_buffer(&self.buffers[slot], 0, bytemuck::bytes_of(&value));
            values[slot] = value;
        }
        &self.buffers[slot]
    }
}

/// Slots of the pass parameters.
mod slot {
    /// Eight passes of the radix sort.
    pub const SORT: usize = 0;
    /// Levels of the scan of the sort histograms.
    pub const SORT_SCAN: usize = 8;
    // Then the levels of the compaction scan, and finally the queries.
}

struct Buffers {
    bodies: [wgpu::Buffer; 2],
    keys: [wgpu::Buffer; 2],
    vals: [wgpu::Buffer; 2],
    counts: wgpu::Buffer,
    scan_sums: Vec<wgpu::Buffer>,
    bbox_partial: wgpu::Buffer,
    bbox: wgpu::Buffer,
    moments_partial: wgpu::Buffer,
    moments: wgpu::Buffer,
    children: wgpu::Buffer,
    parents: wgpu::Buffer,
    node_depth: wgpu::Buffer,
    node_slot: wgpu::Buffer,
    node_list: wgpu::Buffer,
    level_count: wgpu::Buffer,
    level_offset: wgpu::Buffer,
    level_args: wgpu::Buffer,
    level_uniforms: Vec<wgpu::Buffer>,
    nodes: wgpu::Buffer,
    swept: wgpu::Buffer,
    pairs: wgpu::Buffer,
    pair_counter: wgpu::Buffer,
    ops: wgpu::Buffer,
    /// Distant bodies found at cleanup: indices, records (`far_cap` at most),
    /// counter.
    far_index: wgpu::Buffer,
    far: wgpu::Buffer,
    far_counter: wgpu::Buffer,
    index_of: wgpu::Buffer,
    /// Identifiers covered by `index_of`.
    id_cap: u64,
    query_ids: wgpu::Buffer,
    query_results: wgpu::Buffer,
    params: wgpu::Buffer,
    /// Readback buffer of the step (layout: module [`readback`]).
    readback: wgpu::Buffer,
}

/// Direct access to the engine's bodies, to draw them without copy.
///
/// The engine alternates between two buffers (sort, compaction):
/// [`current`](Self::current) designates the one holding the last published
/// state. Since computation and display share the same command queue, a draw
/// submitted after the read always sees a complete set of bodies. The only
/// exception, harmless: during one frame, a step that removes bodies may
/// show a few duplicates.
///
/// Each body takes [`STRIDE`](Self::STRIDE) bytes; position (3 × f32) and
/// mass (f32) are at offsets [`POSITION_OFFSET`](Self::POSITION_OFFSET) and
/// [`MASS_OFFSET`](Self::MASS_OFFSET). A body of zero mass is dead.
pub struct BodiesView {
    buffers: [wgpu::Buffer; 2],
    /// Index of the current buffer (bit 32) and body count (low 32 bits).
    current: AtomicU64,
}

impl BodiesView {
    pub const STRIDE: u64 = std::mem::size_of::<GpuBody>() as u64;
    pub const POSITION_OFFSET: u64 = 0;
    pub const MASS_OFFSET: u64 = 12;

    /// Current buffer and the number of bodies it holds.
    pub fn current(&self) -> (&wgpu::Buffer, u32) {
        let v = self.current.load(Ordering::Acquire);
        (&self.buffers[(v >> 32) as usize & 1], v as u32)
    }

    fn publish(&self, cur: usize, n: usize) {
        self.current
            .store(((cur as u64) << 32) | n as u64, Ordering::Release);
    }
}

/// Identifier lists whose positions are read back at every step.
#[derive(Default)]
struct Queries {
    /// Tracked lists, the most recently requested last.
    lists: Vec<Vec<BodyId>>,
    /// Positions read back, in the order of the concatenated lists.
    results: Vec<Option<DVec3>>,
    /// Step that `results` corresponds to.
    step: Option<u64>,
}

impl Queries {
    fn total(&self) -> usize {
        self.lists.iter().map(Vec::len).sum()
    }

    /// Positions of the list `ids` if it is tracked and up to date.
    fn cached(&self, ids: &[BodyId], step: u64) -> Option<Vec<Option<DVec3>>> {
        if self.step != Some(step) {
            return None;
        }
        let mut start = 0;
        for list in &self.lists {
            if list.as_slice() == ids {
                return Some(self.results[start..start + list.len()].to_vec());
            }
            start += list.len();
        }
        None
    }
}

/// Mergers of a step: operations to apply on the GPU, pairs (absorbed,
/// resulting) and position of each resulting body.
struct MergePlan {
    ops: Vec<MergeOp>,
    merges: Vec<(BodyId, BodyId)>,
    survivors: Vec<(BodyId, DVec3)>,
}

/// Quantities read back at the end of a step.
#[derive(Clone, Copy, Default)]
struct Moments {
    /// (m, m·x), (m·v, ½ m v²), (½ m φ, 0, 0, 0), summed over all bodies.
    raw: [[f32; 4]; 3],
}

/// Barnes-Hut engine on the GPU.
pub struct GpuEngine {
    gpu: Gpu,
    k: Kernels,
    b: Buffers,
    slots: PassSlots,
    params: PhysicsParams,
    capacity: usize,
    pair_cap: usize,
    /// Current capacities of the `far` and `ops` buffers (in elements).
    far_cap: usize,
    ops_cap: usize,
    /// Total initial mass (mass threshold of ejected clusters).
    initial_mass: f64,
    /// External field of the first half kick and of the end of the step.
    field_prev: Option<ExternalField>,
    field_next: Option<ExternalField>,
    /// Index of the current body buffer (double buffering).
    cur: usize,
    n: usize,
    step: u64,
    time: f64,
    /// Buffers exposed to the display (shared device only).
    view: Option<Arc<BodiesView>>,
    moments: Moments,
    queries: RefCell<Queries>,
    /// The index table is stale (compaction since the last tree).
    map_stale: std::cell::Cell<bool>,
    /// Steps since the tree topology was last built (`u32::MAX`: to be
    /// rebuilt).
    tree_age: u32,
}

impl GpuEngine {
    /// Engine on its own device (benchmark, tests, no window).
    pub fn new(state: SimulationState, params: PhysicsParams) -> Result<Self, String> {
        Self::build(Gpu::open()?, state, params, false)
    }

    /// Engine on the interface's device: its bodies can be drawn directly
    /// ([`PhysicsEngine::live_view`]).
    pub fn with_device(
        state: SimulationState,
        params: PhysicsParams,
        shared: &SharedDevice,
    ) -> Result<Self, String> {
        Self::build(Gpu::shared(shared)?, state, params, true)
    }

    fn build(
        gpu: Gpu,
        state: SimulationState,
        mut params: PhysicsParams,
        expose: bool,
    ) -> Result<Self, String> {
        params.theta = params.theta.clamp(0.05, 1.0);
        let n = state.bodies.len();
        if n > MAX_BODIES {
            return Err(format!(
                "le moteur GPU est limité à {MAX_BODIES} corps ({n} demandés)"
            ));
        }
        let max_id = state.bodies.ids.iter().copied().max().unwrap_or(0);
        if max_id >= u32::MAX as u64 {
            return Err("identifiants de corps trop grands pour le moteur GPU".to_owned());
        }
        // Margin for bodies handed over by other subsystems
        // ([`PhysicsEngine::insert`]), if the device allows it.
        let limit = (gpu.limits.max_storage_buffer_binding_size as u64).min(gpu.limits.max_buffer_size);
        let largest_for = |capacity: usize| {
            let body_bytes = (capacity * std::mem::size_of::<GpuBody>()) as u64;
            let node_bytes = (capacity * NODE_BYTES) as u64;
            body_bytes.max(node_bytes).max((max_id + 1) * 4)
        };
        let roomy = (n + (n / 8).max(1024)).min(MAX_BODIES);
        let capacity = if largest_for(roomy) <= limit { roomy } else { n.max(2) };
        let body_bytes = (capacity * std::mem::size_of::<GpuBody>()) as u64;
        let node_bytes = (capacity * NODE_BYTES) as u64;
        let largest = largest_for(capacity);
        if largest > gpu.limits.max_storage_buffer_binding_size as u64
            || largest > gpu.limits.max_buffer_size
        {
            return Err(format!(
                "trop de corps pour cette carte graphique ({} : tampon de {} Mo, limite {} Mo)",
                gpu.adapter_name,
                largest / 1_000_000,
                (gpu.limits.max_storage_buffer_binding_size as u64).min(gpu.limits.max_buffer_size)
                    / 1_000_000
            ));
        }
        let k = Kernels::new(&gpu);
        let pair_cap = capacity.min(MAX_PAIRS);
        let wg = groups(capacity) as u64;

        // Scan workspace: the larger of its two uses (sort histograms: 16 ×
        // workgroups; compaction flags: n).
        let scan_len = (capacity as u64).max(16 * wg);
        let mut scan_sums = Vec::new();
        let mut len = scan_len;
        loop {
            let blocks = len.div_ceil(WG as u64);
            scan_sums.push(gpu.storage("scan-sums", blocks * 4));
            if blocks <= 1 {
                break;
            }
            len = blocks;
        }
        let scan_levels = scan_sums.len();
        let slots = PassSlots::new(&gpu, slot::SORT_SCAN + 2 * scan_levels + 1);

        let level_uniforms = (0..LEVELS)
            .map(|l| {
                gpu.uniform(
                    "level",
                    bytemuck::bytes_of(&Pass {
                        n: 0,
                        a: l as u32,
                        b: 0,
                        c: 0,
                    }),
                )
            })
            .collect();
        let cap = capacity as u64;
        let b = Buffers {
            bodies: [
                gpu.storage_vertex("bodies-a", body_bytes),
                gpu.storage_vertex("bodies-b", body_bytes),
            ],
            keys: [
                gpu.storage("keys-a", cap * 4),
                gpu.storage("keys-b", cap * 4),
            ],
            vals: [
                gpu.storage("vals-a", cap * 4),
                gpu.storage("vals-b", cap * 4),
            ],
            counts: gpu.storage("counts", scan_len * 4),
            scan_sums,
            bbox_partial: gpu.storage("bbox-partial", wg * 32),
            bbox: gpu.storage("bbox", 32),
            moments_partial: gpu.storage("moments-partial", wg * 48),
            moments: gpu.storage("moments", 48),
            children: gpu.storage("children", cap * 8),
            parents: gpu.storage("parents", cap * 8),
            node_depth: gpu.storage("node-depth", cap * 4),
            node_slot: gpu.storage("node-slot", cap * 4),
            node_list: gpu.storage("node-list", cap * 4),
            level_count: gpu.storage("level-count", LEVELS as u64 * 4),
            level_offset: gpu.storage("level-offset", LEVELS as u64 * 4),
            level_args: gpu.indirect("level-args", LEVELS as u64 * 12),
            level_uniforms,
            nodes: gpu.storage("nodes", node_bytes),
            swept: gpu.storage("swept", cap * 32),
            pairs: gpu.storage("pairs", pair_cap as u64 * PAIR_BYTES as u64),
            pair_counter: gpu.storage("pair-counter", 16),
            ops: gpu.storage("ops", 2 * pair_cap as u64 * OP_BYTES),
            far_index: gpu.storage("far-index", cap * 4),
            far: gpu.storage("far", (FAR_INITIAL.min(capacity) * BODY_BYTES) as u64),
            far_counter: gpu.storage("far-counter", 16),
            index_of: gpu.storage("index-of", (max_id + 1) * 4),
            id_cap: max_id + 1,
            query_ids: gpu.storage("query-ids", QUERY_CAP as u64 * 4),
            query_results: gpu.storage("query-results", QUERY_CAP as u64 * 16),
            params: gpu.uniform("params", bytemuck::bytes_of(&Params::default())),
            readback: gpu.readback("readback", readback::SIZE),
        };

        // Upload of the initial state.
        let bodies: Vec<GpuBody> = (0..n)
            .map(|i| {
                let s = &state.bodies;
                GpuBody {
                    pos: f3(s.positions[i]),
                    mass: s.masses[i] as f32,
                    vel: f3(s.velocities[i]),
                    id: s.ids[i] as u32,
                    ..Default::default()
                }
            })
            .collect();
        if !bodies.is_empty() {
            gpu.queue
                .write_buffer(&b.bodies[0], 0, bytemuck::cast_slice(&bodies));
        }

        let view = expose.then(|| {
            Arc::new(BodiesView {
                buffers: b.bodies.clone(),
                current: AtomicU64::new(0),
            })
        });
        let mut engine = Self {
            gpu,
            k,
            b,
            slots,
            params,
            capacity,
            pair_cap,
            far_cap: FAR_INITIAL.min(capacity),
            ops_cap: 2 * pair_cap,
            initial_mass: state
                .initial_mass
                .unwrap_or_else(|| state.bodies.masses.iter().sum::<f64>()),
            field_prev: None,
            field_next: None,
            cur: 0,
            n,
            step: state.step,
            time: state.time,
            view,
            moments: Moments::default(),
            queries: RefCell::default(),
            map_stale: Default::default(),
            tree_age: u32::MAX,
        };
        // First computation of the field (the first kick needs it) and of the moments.
        engine.write_params(0);
        let mut enc = engine.gpu.encoder();
        engine.encode_tree_and_forces(&mut enc);
        engine.encode_moments(&mut enc);
        enc.copy_buffer_to_buffer(
            &engine.b.moments,
            0,
            &engine.b.readback,
            readback::MOMENTS,
            48,
        );
        let index = engine.gpu.submit(enc);
        let mut raw = [[0f32; 4]; 3];
        engine
            .gpu
            .map_read(&engine.b.readback, index, readback::SIZE, |r| {
                raw = read_moments(r)
            });
        engine.moments = Moments { raw };
        engine.publish_view();
        Ok(engine)
    }

    /// Points the display to the current buffer (after the work is submitted).
    fn publish_view(&self) {
        if let Some(v) = &self.view {
            v.publish(self.cur, self.n);
        }
    }

    /// Name of the graphics card in use.
    pub fn adapter_name(&self) -> &str {
        &self.gpu.adapter_name
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Cumulative GPU times per kernel (ms, dispatches), if profiling is on
    /// (`STELLAR_GPU_PROFILE=1`, engine on its own device).
    pub fn profile(&self) -> Vec<(&'static str, f64, u64)> {
        self.gpu.profile()
    }

    fn write_params(&self, n_ops: usize) {
        let p = &self.params;
        let limit = p.cleanup_distance_factor * p.reference_radius;
        let params = Params {
            n: self.n as u32,
            num_wg: groups(self.n),
            cap: self.pair_cap as u32,
            n_ops: n_ops as u32,
            dt: p.dt as f32,
            half_dt: (0.5 * p.dt) as f32,
            eps2: (p.softening * p.softening) as f32,
            inv_theta: (1.0 / p.theta.max(1e-3)) as f32,
            r0: p.base_radius as f32,
            g: p.g as f32,
            limit2: (limit * limit).min(f32::MAX as f64) as f32,
            pad: 0.0,
            field_prev: GpuField::of(self.field_prev.or(self.field_next)),
            field_next: GpuField::of(self.field_next),
        };
        self.gpu
            .queue
            .write_buffer(&self.b.params, 0, bytemuck::bytes_of(&params));
    }

    /// In-place exclusive scan of `len` integers of `data` (recursive);
    /// `slot_base`: parameter slots, one per level.
    fn encode_scan(
        &self,
        enc: &mut gpu::Encoder,
        data: &wgpu::Buffer,
        len: usize,
        level: usize,
        slot_base: usize,
    ) {
        let wg = groups(len);
        let u = self.slots.get(
            &self.gpu,
            slot_base + level,
            Pass {
                n: len as u32,
                ..Default::default()
            },
        );
        let sums = &self.b.scan_sums[level];
        self.gpu
            .run(enc, &self.k.scan_blocks, &[u, data, sums], wg);
        if wg > 1 {
            self.encode_scan(enc, sums, wg as usize, level + 1, slot_base);
            self.gpu.run(enc, &self.k.scan_add, &[u, data, sums], wg);
        }
    }

    /// Radix sort (30-bit key, 4 bits per pass) of keys[0]/vals[0].
    fn encode_sort(&self, enc: &mut gpu::Encoder) {
        let n = self.n;
        let wg = groups(n);
        let b = &self.b;
        let mut src = 0;
        for pass in 0..8u32 {
            let u = self.slots.get(
                &self.gpu,
                slot::SORT + pass as usize,
                Pass {
                    n: n as u32,
                    a: 4 * pass,
                    b: wg,
                    c: 0,
                },
            );
            let dst = 1 - src;
            self.gpu
                .run(enc, &self.k.radix_hist, &[u, &b.keys[src], &b.counts], wg);
            self.encode_scan(enc, &b.counts, 16 * wg as usize, 0, slot::SORT_SCAN);
            self.gpu.run(
                enc,
                &self.k.radix_scatter,
                &[
                    u,
                    &b.keys[src],
                    &b.vals[src],
                    &b.counts,
                    &b.keys[dst],
                    &b.vals[dst],
                ],
                wg,
            );
            src = dst;
        }
        debug_assert_eq!(src, 0);
    }

    /// Reorders the bodies (Morton) and builds the tree, or only refits it (see
    /// [`REBUILD_EVERY`]), then computes the field and keeps the index table up
    /// to date.
    fn encode_tree_and_forces(&mut self, enc: &mut gpu::Encoder) {
        let n = self.n;
        if n == 0 {
            return;
        }
        let wg = groups(n);
        let mut cur = self.cur;
        let rebuild = self.tree_age >= REBUILD_EVERY;
        self.tree_age = if rebuild { 0 } else { self.tree_age + 1 };
        if n >= 2 && rebuild {
            let (g, k, b) = (&self.gpu, &self.k, &self.b);
            let p = &b.params;
            let src = &b.bodies[cur];
            let dst = &b.bodies[1 - cur];
            g.run(enc, &k.bbox_partial, &[p, src, &b.bbox_partial], wg);
            g.run(enc, &k.bbox_final, &[p, &b.bbox_partial, &b.bbox], 1);
            g.run(
                enc,
                &k.morton,
                &[p, src, &b.bbox, &b.keys[0], &b.vals[0]],
                wg,
            );
            self.encode_sort(enc);
            g.run(enc, &k.gather, &[p, &b.vals[0], src, dst], wg);
            cur = 1 - cur;
            g.run(
                enc,
                &k.lbvh_build,
                &[p, &b.keys[0], &b.children, &b.parents],
                wg,
            );
            enc.clear_buffer(&b.level_count, 0, None);
            g.run(
                enc,
                &k.lbvh_depth,
                &[p, &b.parents, &b.level_count, &b.node_depth, &b.node_slot],
                wg,
            );
            g.run(
                enc,
                &k.lbvh_levels,
                &[&b.level_count, &b.level_offset, &b.level_args],
                1,
            );
            g.run(
                enc,
                &k.lbvh_list,
                &[
                    p,
                    &b.node_depth,
                    &b.node_slot,
                    &b.level_offset,
                    &b.node_list,
                ],
                wg,
            );
        }
        if n >= 2 {
            let (g, k, b) = (&self.gpu, &self.k, &self.b);
            let p = &b.params;
            let bodies = &b.bodies[cur];
            for level in (0..LEVELS).rev() {
                g.run_indirect(
                    enc,
                    &k.lbvh_aggregate,
                    &[
                        p,
                        &b.level_uniforms[level],
                        bodies,
                        &b.children,
                        &b.level_count,
                        &b.level_offset,
                        &b.node_list,
                        &b.nodes,
                        &b.swept,
                    ],
                    &b.level_args,
                    level as u64 * 12,
                );
            }
        }
        let b = &self.b;
        self.gpu.run_2d(
            enc,
            &self.k.forces,
            &[&b.params, &b.bodies[cur], &b.nodes, &b.children],
            grid(n, FORCE_GROUP),
        );
        self.cur = cur;
        if rebuild || self.map_stale.get() {
            self.encode_index_map(enc);
            self.map_stale.set(false);
        }
    }

    fn encode_index_map(&self, enc: &mut gpu::Encoder) {
        let b = &self.b;
        self.gpu.run(
            enc,
            &self.k.index_map,
            &[&b.params, &b.bodies[self.cur], &b.index_of],
            groups(self.n),
        );
    }

    /// Global moments (mass, center of mass, momentum, energies).
    fn encode_moments(&self, enc: &mut gpu::Encoder) {
        let b = &self.b;
        let wg = groups(self.n);
        self.gpu.run(
            enc,
            &self.k.moments_partial,
            &[&b.params, &b.bodies[self.cur], &b.moments_partial],
            wg,
        );
        self.gpu.run(
            enc,
            &self.k.moments_final,
            &[&b.params, &b.moments_partial, &b.moments],
            1,
        );
    }

    /// Positions of the tracked lists (at most `QUERY_CAP` identifiers, already
    /// written to `query_ids`).
    fn encode_queries(&self, enc: &mut gpu::Encoder, count: usize) {
        if count == 0 {
            return;
        }
        let b = &self.b;
        let u = self.slots.get(
            &self.gpu,
            slot::SORT_SCAN + 2 * self.b.scan_sums.len(),
            Pass {
                n: count as u32,
                ..Default::default()
            },
        );
        self.gpu.run(
            enc,
            &self.k.query,
            &[
                &b.params,
                u,
                &b.query_ids,
                &b.index_of,
                &b.bodies[self.cur],
                &b.query_results,
            ],
            groups(count),
        );
    }

    /// Removes the bodies of zero mass (compaction by scan).
    fn encode_compaction(&mut self, enc: &mut gpu::Encoder) {
        let b = &self.b;
        let wg = groups(self.n);
        // The flags reuse the buffer of the sort histograms.
        self.gpu.run(
            enc,
            &self.k.alive_flags,
            &[&b.params, &b.bodies[self.cur], &b.counts],
            wg,
        );
        self.encode_scan(
            enc,
            &b.counts,
            self.n,
            0,
            slot::SORT_SCAN + b.scan_sums.len(),
        );
        self.gpu.run(
            enc,
            &self.k.compact,
            &[
                &b.params,
                &b.bodies[self.cur],
                &b.counts,
                &b.bodies[1 - self.cur],
            ],
            wg,
        );
        self.cur = 1 - self.cur;
    }

    /// Groups the pairs in contact and prepares the merge operations.
    ///
    /// Bodies removed by the cleanup in the same step are left out of the pairs
    /// (they are already gone). Also returns, for each group, the identifier of
    /// the resulting body and its position.
    fn plan_merges(&self, pairs: &[PairRec], removed: &HashSet<BodyId>) -> MergePlan {
        let mut index_of: HashMap<u32, usize> = HashMap::new();
        let mut members: Vec<(u32, GpuBody)> = Vec::new();
        let mut slot = |i: u32, body: GpuBody, members: &mut Vec<(u32, GpuBody)>| -> usize {
            *index_of.entry(i).or_insert_with(|| {
                members.push((i, body));
                members.len() - 1
            })
        };
        let mut links = Vec::with_capacity(pairs.len());
        for p in pairs {
            if removed.contains(&(p.a.id as u64)) || removed.contains(&(p.b.id as u64)) {
                continue;
            }
            let a = slot(p.ia, p.a, &mut members);
            let b = slot(p.ib, p.b, &mut members);
            links.push((a, b));
        }
        // Union-find on local indices.
        let mut parent: Vec<usize> = (0..members.len()).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        for (a, b) in links {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for m in 0..members.len() {
            let r = find(&mut parent, m);
            groups.entry(r).or_default().push(m);
        }

        let mut ops = Vec::new();
        let mut merges = Vec::new();
        let mut survivors = Vec::new();
        for (_, group) in groups {
            let inputs: Vec<MergeInput> = group
                .iter()
                .map(|&m| {
                    let b = &members[m].1;
                    MergeInput {
                        id: b.id as u64,
                        mass: b.mass as f64,
                        position: v3(b.pos),
                        velocity: v3(b.vel),
                    }
                })
                .collect();
            let merged = merge(&inputs);
            survivors.push((merged.id, merged.position));
            let acc: DVec3 = group
                .iter()
                .map(|&m| v3(members[m].1.acc) * members[m].1.mass as f64)
                .sum::<DVec3>()
                / merged.mass;
            for &m in &group {
                let (index, body) = members[m];
                if body.id as u64 == merged.id {
                    ops.push(MergeOp {
                        index,
                        kind: 0,
                        pad: [0; 2],
                        body: GpuBody {
                            pos: f3(merged.position),
                            mass: merged.mass as f32,
                            vel: f3(merged.velocity),
                            id: body.id,
                            acc: f3(acc),
                            pot: body.pot,
                        },
                    });
                } else {
                    ops.push(MergeOp {
                        index,
                        kind: 1,
                        pad: [0; 2],
                        body,
                    });
                    merges.push((body.id as u64, merged.id));
                }
            }
        }
        MergePlan {
            ops,
            merges,
            survivors,
        }
    }

    fn step_inner(&mut self) -> StepReport {
        let mut report = StepReport::default();
        self.step += 1;
        self.time += self.params.dt;
        if self.n == 0 {
            self.queries.borrow_mut().step = Some(self.step);
            self.moments = Moments::default();
            return report;
        }
        let wg = groups(self.n);
        let period = self.params.cleanup_period as u64;
        let cleanup = period > 0 && self.step.is_multiple_of(period);

        // 1. Kick ½, drift, tree, field, contacts, kick ½, cleanup, moments and
        //    tracked positions: one submission, one wait.
        self.write_params(0);
        let mut enc = self.gpu.encoder();
        {
            let b = &self.b;
            self.gpu.run(
                &mut enc,
                &self.k.kick_drift,
                &[&b.params, &b.bodies[self.cur]],
                wg,
            );
        }
        self.encode_tree_and_forces(&mut enc);
        {
            let b = &self.b;
            enc.clear_buffer(&b.pair_counter, 0, None);
            self.gpu.run(
                &mut enc,
                &self.k.collide,
                &[
                    &b.params,
                    &b.bodies[self.cur],
                    &b.swept,
                    &b.children,
                    &b.pairs,
                    &b.pair_counter,
                ],
                wg,
            );
            // Kick ½ and global moments in a single read of the bodies.
            self.gpu.run(
                &mut enc,
                &self.k.kick_moments,
                &[&b.params, &b.bodies[self.cur], &b.moments_partial],
                wg,
            );
            self.gpu.run(
                &mut enc,
                &self.k.moments_final,
                &[&b.params, &b.moments_partial, &b.moments],
                1,
            );
        }
        if cleanup {
            self.encode_far(&mut enc);
        }
        let query_count = self.queries.borrow().total();
        self.encode_queries(&mut enc, query_count);
        let index = {
            let b = &self.b;
            let rb = &b.readback;
            enc.copy_buffer_to_buffer(&b.pair_counter, 0, rb, readback::COUNTERS, 4);
            enc.copy_buffer_to_buffer(&b.far_counter, 0, rb, readback::COUNTERS + 4, 4);
            enc.copy_buffer_to_buffer(&b.moments, 0, rb, readback::MOMENTS, 48);
            if query_count > 0 {
                enc.copy_buffer_to_buffer(
                    &b.query_results,
                    0,
                    rb,
                    readback::QUERY,
                    16 * query_count as u64,
                );
            }
            let pairs = (PAIR_PREFIX.min(self.pair_cap) * PAIR_BYTES) as u64;
            enc.copy_buffer_to_buffer(&b.pairs, 0, rb, readback::PAIRS, pairs);
            if cleanup {
                let k = FAR_PREFIX.min(self.far_cap) as u64;
                enc.copy_buffer_to_buffer(&b.far_index, 0, rb, readback::FAR_INDEX, 4 * k);
                enc.copy_buffer_to_buffer(&b.far, 0, rb, readback::FAR, k * BODY_BYTES as u64);
            }
            self.gpu.submit(enc)
        };
        self.publish_view();

        // 2. Readback: counters, moments, positions, first pairs.
        let mut found = 0usize;
        let mut far_count = 0usize;
        let mut pairs: Vec<PairRec> = Vec::new();
        let mut far_index: Vec<u32> = Vec::new();
        let mut far: Vec<GpuBody> = Vec::new();
        let mut query_raw: Vec<[f32; 4]> = Vec::new();
        let mut moments = [[0f32; 4]; 3];
        self.gpu
            .map_read(&self.b.readback, index, readback::SIZE, |raw| {
                found = u32_at(raw, readback::COUNTERS as usize) as usize;
                far_count = u32_at(raw, readback::COUNTERS as usize + 4) as usize;
                moments = read_moments(raw);
                let q = readback::QUERY as usize;
                query_raw = bytemuck::cast_slice(&raw[q..q + 16 * query_count]).to_vec();
                let count = found.min(self.pair_cap).min(PAIR_PREFIX);
                let p = readback::PAIRS as usize;
                pairs = bytemuck::cast_slice(&raw[p..p + count * PAIR_BYTES]).to_vec();
                if cleanup {
                    let count = far_count.min(FAR_PREFIX).min(self.far_cap);
                    let r = readback::FAR_INDEX as usize;
                    far_index = bytemuck::cast_slice(&raw[r..r + 4 * count]).to_vec();
                    let r = readback::FAR as usize;
                    far = bytemuck::cast_slice(&raw[r..r + BODY_BYTES * count]).to_vec();
                }
            });
        // Beyond the first records (rare): read back the rest.
        let total_pairs = found.min(self.pair_cap);
        if total_pairs > pairs.len() {
            let start = pairs.len();
            let raw = self.gpu.read(
                &self.b.pairs,
                (start * PAIR_BYTES) as u64,
                ((total_pairs - start) * PAIR_BYTES) as u64,
            );
            pairs.extend_from_slice(bytemuck::cast_slice(&raw));
        }
        self.moments = Moments { raw: moments };

        // 3. Cleanup: the domain rule sorts out the distant bodies.
        let mut kills = Vec::new();
        if cleanup {
            if far_count > far.len() {
                self.read_far(far_count, &mut far_index, &mut far);
            }
            let (ops, clusters) = self.sort_out_far(&far_index, &far);
            report.removed = ops.iter().map(|op| op.body.id as u64).collect();
            for op in &ops {
                let b = &op.body;
                report
                    .removed_bodies
                    .push(b.id as u64, v3(b.pos), v3(b.vel), b.mass as f64);
            }
            report.clusters = Some(clusters);
            kills = ops;
        }

        // 4. Mergers: grouped on the CPU, applied without waiting.
        let mut dead = kills.len();
        let mut survivors = Vec::new();
        let mut ops = Vec::new();
        if !pairs.is_empty() {
            let removed: HashSet<BodyId> = report.removed.iter().copied().collect();
            let plan = self.plan_merges(&pairs, &removed);
            dead += plan.merges.len();
            report.merges = plan.merges;
            ops = plan.ops;
            survivors = plan.survivors;
        }
        ops.extend(kills);
        if dead > 0 {
            if ops.len() > self.ops_cap {
                self.gpu.forget(&self.b.ops);
                self.ops_cap = ops.len().next_power_of_two();
                self.b.ops = self.gpu.storage("ops", self.ops_cap as u64 * OP_BYTES);
            }
            self.write_params(ops.len());
            let mut enc = self.gpu.encoder();
            if !ops.is_empty() {
                self.gpu
                    .queue
                    .write_buffer(&self.b.ops, 0, bytemuck::cast_slice(&ops));
                let b = &self.b;
                self.gpu.run(
                    &mut enc,
                    &self.k.apply_merges,
                    &[&b.params, &b.ops, &b.bodies[self.cur]],
                    groups(ops.len()),
                );
            }
            self.encode_compaction(&mut enc);
            self.gpu.submit(enc);
            // The following parameters only apply from the next submission, once the
            // compaction is done.
            self.n -= dead.min(self.n);
            self.write_params(0);
            self.map_stale.set(true);
            self.tree_age = u32::MAX;
            self.publish_view();
        }

        // 5. Tracked positions, corrected for the step's mergers and removals.
        let gone: HashSet<BodyId> = report
            .merges
            .iter()
            .map(|&(a, _)| a)
            .chain(report.removed.iter().copied())
            .collect();
        let moved: HashMap<BodyId, DVec3> = survivors.into_iter().collect();
        let mut q = self.queries.borrow_mut();
        let ids: Vec<BodyId> = q.lists.iter().flatten().copied().collect();
        q.results = ids
            .iter()
            .zip(&query_raw)
            .map(|(id, r)| {
                if gone.contains(id) {
                    None
                } else if let Some(&p) = moved.get(id) {
                    Some(p)
                } else {
                    (r[3] > 0.5).then(|| DVec3::new(r[0] as f64, r[1] as f64, r[2] as f64))
                }
            })
            .collect();
        q.step = Some(self.step);
        self.field_prev = self.field_next;
        report
    }

    /// Finds the distant bodies (beyond k × R from the barycenter): indices and
    /// records (up to `far_cap`), without removing anything.
    fn encode_far(&self, enc: &mut gpu::Encoder) {
        let b = &self.b;
        enc.clear_buffer(&b.far_counter, 0, None);
        self.gpu.run(
            enc,
            &self.k.cleanup,
            &[
                &b.params,
                &b.bodies[self.cur],
                &b.moments,
                &b.far_index,
                &b.far,
                &b.far_counter,
            ],
            groups(self.n),
        );
    }

    /// Reads back all distant bodies when the step's readback held only part of
    /// them; grows their buffer if needed (the bodies have not moved since: the
    /// search is simply redone).
    fn read_far(&mut self, count: usize, index: &mut Vec<u32>, far: &mut Vec<GpuBody>) {
        if count > self.far_cap {
            self.gpu.forget(&self.b.far);
            self.far_cap = count.next_power_of_two().min(self.capacity);
            self.b.far = self.gpu.storage("far", (self.far_cap * BODY_BYTES) as u64);
            let mut enc = self.gpu.encoder();
            self.encode_far(&mut enc);
            self.gpu.submit(enc);
        }
        let raw = self.gpu.read(&self.b.far_index, 0, 4 * count as u64);
        *index = bytemuck::cast_slice(&raw).to_vec();
        let raw = self.gpu.read(&self.b.far, 0, (count * BODY_BYTES) as u64);
        *far = bytemuck::cast_slice(&raw).to_vec();
    }

    /// Applies the domain rule to the distant bodies; returns the removal
    /// operations (zero mass) and the kept clusters. The moments read back are
    /// corrected for the removed bodies.
    fn sort_out_far(
        &mut self,
        index: &[u32],
        far: &[GpuBody],
    ) -> (Vec<MergeOp>, Vec<EjectedCluster>) {
        let mut raw = self.moments.raw;
        let mass = raw[0][0] as f64;
        if far.is_empty() || mass <= 0.0 {
            return (Vec::new(), Vec::new());
        }
        let velocity = DVec3::new(raw[1][0] as f64, raw[1][1] as f64, raw[1][2] as f64) / mass;
        let candidates: Vec<Candidate> = far
            .iter()
            .map(|b| Candidate {
                id: b.id as u64,
                position: v3(b.pos),
                velocity: v3(b.vel),
                mass: b.mass as f64,
                potential: b.pot as f64,
            })
            .collect();
        let rules = EjectionRules::new(&self.params, self.initial_mass);
        let ejection = sort_out(&candidates, velocity, &rules);
        let ops = ejection
            .removed
            .iter()
            .map(|&k| {
                let body = far[k];
                let (mb, v) = (body.mass, body.vel);
                raw[0][0] -= mb;
                for a in 0..3 {
                    raw[0][a + 1] -= mb * body.pos[a];
                    raw[1][a] -= mb * v[a];
                }
                raw[1][3] -= 0.5 * mb * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
                raw[2][0] -= mb * body.pot;
                MergeOp {
                    index: index[k],
                    kind: 1,
                    pad: [0; 2],
                    body,
                }
            })
            .collect();
        self.moments = Moments { raw };
        (ops, ejection.clusters)
    }

    /// Reads back the positions of every tracked list right away.
    fn refresh_queries(&self) {
        let mut q = self.queries.borrow_mut();
        let ids: Vec<u32> = q
            .lists
            .iter()
            .flatten()
            .map(|&id| id.min(u32::MAX as u64) as u32)
            .collect();
        if !ids.is_empty() {
            self.gpu
                .queue
                .write_buffer(&self.b.query_ids, 0, bytemuck::cast_slice(&ids));
        }
        let mut enc = self.gpu.encoder();
        if self.map_stale.replace(false) {
            self.encode_index_map(&mut enc);
        }
        self.encode_queries(&mut enc, ids.len());
        self.gpu.submit(enc);
        let raw = self.gpu.read(&self.b.query_results, 0, 16 * ids.len() as u64);
        let r: &[[f32; 4]] = bytemuck::cast_slice(&raw);
        q.results = r
            .iter()
            .map(|v| (v[3] > 0.5).then(|| DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)))
            .collect();
        q.step = Some(self.step);
    }

    /// Reads back every body (engine's internal order).
    pub fn read_bodies(&self) -> Vec<GpuBody> {
        if self.n == 0 {
            return Vec::new();
        }
        let raw = self.gpu.read(
            &self.b.bodies[self.cur],
            0,
            (self.n * std::mem::size_of::<GpuBody>()) as u64,
        );
        bytemuck::cast_slice::<u8, GpuBody>(&raw).to_vec()
    }
}

fn read_moments(raw: &[u8]) -> [[f32; 4]; 3] {
    let m = readback::MOMENTS as usize;
    let f: &[f32] = bytemuck::cast_slice(&raw[m..m + 48]);
    [
        [f[0], f[1], f[2], f[3]],
        [f[4], f[5], f[6], f[7]],
        [f[8], f[9], f[10], f[11]],
    ]
}

impl PhysicsEngine for GpuEngine {
    fn step(&mut self) -> StepReport {
        self.step_inner()
    }

    fn state(&self) -> SimulationState {
        let mut s = SimulationState::default();
        self.write_state(&mut s);
        s
    }

    fn write_state(&self, out: &mut SimulationState) {
        // Direct conversion from the mapped memory, in parallel, into the existing
        // arrays of `out` (no intermediate copy).
        let b: &mut BodySet = &mut out.bodies;
        let n = self.n;
        b.ids.resize(n, 0);
        b.positions.resize(n, DVec3::ZERO);
        b.velocities.resize(n, DVec3::ZERO);
        b.masses.resize(n, 0.0);
        if n > 0 {
            let size = (n * std::mem::size_of::<GpuBody>()) as u64;
            self.gpu
                .read_with(&self.b.bodies[self.cur], 0, size, |raw| {
                    let bodies: &[GpuBody] = bytemuck::cast_slice(raw);
                    rayon::join(
                        || {
                            rayon::join(
                                || {
                                    b.ids
                                        .par_iter_mut()
                                        .zip(bodies.par_iter())
                                        .for_each(|(d, g)| *d = g.id as u64)
                                },
                                || {
                                    b.masses
                                        .par_iter_mut()
                                        .zip(bodies.par_iter())
                                        .for_each(|(d, g)| *d = g.mass as f64)
                                },
                            )
                        },
                        || {
                            rayon::join(
                                || {
                                    b.positions
                                        .par_iter_mut()
                                        .zip(bodies.par_iter())
                                        .for_each(|(d, g)| *d = v3(g.pos))
                                },
                                || {
                                    b.velocities
                                        .par_iter_mut()
                                        .zip(bodies.par_iter())
                                        .for_each(|(d, g)| *d = v3(g.vel))
                                },
                            )
                        },
                    );
                });
        }
        out.step = self.step;
        out.time = self.time;
    }

    fn body_count(&self) -> usize {
        self.n
    }

    fn positions_of(&self, ids: &[BodyId]) -> Vec<Option<DVec3>> {
        if let Some(r) = self.queries.borrow().cached(ids, self.step) {
            return r;
        }
        if ids.is_empty() {
            return Vec::new();
        }
        if ids.len() > QUERY_CAP {
            // List too long to be tracked: read through the state.
            let s = self.state();
            let index: HashMap<BodyId, usize> =
                s.bodies.ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
            return ids
                .iter()
                .map(|id| index.get(id).map(|&i| s.bodies.positions[i]))
                .collect();
        }
        // New list: tracked at every step from now on (replacing the oldest one),
        // and read back right away.
        {
            let mut q = self.queries.borrow_mut();
            q.lists.retain(|l| l.as_slice() != ids);
            q.lists.push(ids.to_vec());
            while q.lists.len() > QUERY_LISTS || q.total() > QUERY_CAP {
                q.lists.remove(0);
            }
        }
        self.refresh_queries();
        self.queries
            .borrow()
            .cached(ids, self.step)
            .expect("tracked list")
    }

    fn center_of_mass(&self) -> Option<DVec3> {
        let m = self.moments.raw;
        let mass = m[0][0] as f64;
        (self.n > 0 && mass > 0.0)
            .then(|| DVec3::new(m[0][1] as f64, m[0][2] as f64, m[0][3] as f64) / mass)
    }

    fn moments(&self) -> SystemMoments {
        let m = self.moments.raw;
        let mass = m[0][0] as f64;
        if self.n == 0 || mass <= 0.0 {
            return SystemMoments::default();
        }
        SystemMoments {
            mass,
            center: DVec3::new(m[0][1] as f64, m[0][2] as f64, m[0][3] as f64) / mass,
            velocity: DVec3::new(m[1][0] as f64, m[1][1] as f64, m[1][2] as f64) / mass,
        }
    }

    fn set_external_field(&mut self, field: ExternalField) -> bool {
        self.field_next = Some(field);
        true
    }

    fn insert(&mut self, bodies: &BodySet) -> bool {
        let k = bodies.len();
        if k == 0 {
            return true;
        }
        if self.n + k > self.capacity {
            return false;
        }
        let max_id = bodies.ids.iter().copied().max().unwrap_or(0);
        if max_id >= u32::MAX as u64 {
            return false;
        }
        if max_id >= self.b.id_cap {
            let cap = (max_id + 1).max(self.b.id_cap * 2);
            if cap * 4 > self.gpu.limits.max_storage_buffer_binding_size as u64 {
                return false;
            }
            self.gpu.forget(&self.b.index_of);
            self.b.index_of = self.gpu.storage("index-of", cap * 4);
            self.b.id_cap = cap;
        }
        let mut raw = self.moments.raw;
        let records: Vec<GpuBody> = (0..k)
            .map(|i| {
                let (m, p, v) = (
                    bodies.masses[i] as f32,
                    f3(bodies.positions[i]),
                    f3(bodies.velocities[i]),
                );
                raw[0][0] += m;
                for a in 0..3 {
                    raw[0][a + 1] += m * p[a];
                    raw[1][a] += m * v[a];
                }
                raw[1][3] += 0.5 * m * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
                GpuBody {
                    pos: p,
                    mass: m,
                    vel: v,
                    id: bodies.ids[i] as u32,
                    ..Default::default()
                }
            })
            .collect();
        self.gpu.queue.write_buffer(
            &self.b.bodies[self.cur],
            (self.n * BODY_BYTES) as u64,
            bytemuck::cast_slice(&records),
        );
        self.moments = Moments { raw };
        self.n += k;
        self.write_params(0);
        self.map_stale.set(true);
        self.tree_age = u32::MAX;
        self.publish_view();
        true
    }

    fn diagnostics(&self) -> Diagnostics {
        let m = self.moments.raw;
        Diagnostics {
            kinetic_energy: m[1][3] as f64,
            potential_energy: m[2][0] as f64,
        }
    }

    fn live_view(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.view
            .as_ref()
            .map(|v| Arc::clone(v) as Arc<dyn Any + Send + Sync>)
    }
}
