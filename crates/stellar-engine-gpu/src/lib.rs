//! # stellar-engine-gpu
//!
//! Moteur de calcul sur carte graphique (shaders de calcul wgpu : Vulkan,
//! DirectX 12, Metal), même algorithme que le moteur CPU :
//!
//! - **Barnes-Hut sur LBVH** : à chaque pas, boîte englobante, codes de
//!   Morton (30 bits), tri radix GPU, construction parallèle de l'arbre
//!   binaire radix (Karras 2012), agrégation niveau par niveau (centres de
//!   masse, boîtes, boîtes balayées), puis parcours par corps ;
//! - **Leapfrog KDK** à pas fixe ;
//! - **fusions balayées** : les paires en contact sont détectées sur le GPU
//!   (même arbre, boîtes balayées) ; le regroupement (union-find, rarement
//!   plus de quelques milliers de paires) est fait sur le CPU, puis appliqué
//!   sur le GPU ;
//! - **nettoyage** des corps éjectés et compaction sur le GPU.
//!
//! Les calculs sont en simple précision (f32) ; l'état est relu en double
//! précision quand l'application le demande (sauvegarde, enregistrement…).
//!
//! Construit sur la carte graphique de l'interface ([`GpuEngineFactory::attach`]),
//! le moteur expose ses tampons de corps ([`BodiesView`]) : l'affichage les
//! lit directement, sans aller-retour par la mémoire centrale.

mod gpu;

use std::any::Any;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use rayon::prelude::*;
use stellar_domain::physics::{merge, MergeInput};
use stellar_domain::{
    BodyId, BodySet, Diagnostics, EngineDescriptor, EngineFactory, PhysicsEngine, PhysicsParams,
    SimulationState, StepReport,
};

use crate::gpu::{groups, Gpu, Kernel};

pub use crate::gpu::{SharedDevice, WG};

pub const ENGINE_ID: &str = "gpu-barnes-hut";

/// Nombre maximal de niveaux de l'arbre (clés 30 bits + départage 32 bits).
const LEVELS: usize = 72;
/// Nombre maximal de paires de contact traitées par pas.
const MAX_PAIRS: usize = 1 << 18;
/// Nombre maximal de corps (lancements 1D : 65 535 groupes de 256).
pub const MAX_BODIES: usize = 65_535 * WG as usize;

// ----------------------------------------------------------------------
// Structures partagées avec les shaders (disposition identique)
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
    pub old: [f32; 3],
    pub flag: u32,
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
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
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

fn v3(a: [f32; 3]) -> DVec3 {
    DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64)
}

fn f3(v: DVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

// ----------------------------------------------------------------------
// Fabrique
// ----------------------------------------------------------------------

/// Fabrique du moteur GPU.
///
/// Par défaut, chaque moteur ouvre sa propre carte graphique. Une fois
/// [`attach`](Self::attach) appelé avec celle de l'interface, les moteurs
/// créés la partagent et exposent leurs corps pour un affichage sans copie.
#[derive(Default)]
pub struct GpuEngineFactory {
    shared: OnceLock<SharedDevice>,
}

impl GpuEngineFactory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Vérifie qu'une carte graphique utilisable est présente ; renvoie son nom.
    pub fn probe() -> Result<String, String> {
        Gpu::open().map(|g| g.adapter_name)
    }

    /// Partage la carte graphique de l'interface avec les moteurs créés
    /// ensuite (sans effet si une carte est déjà attachée).
    pub fn attach(&self, shared: SharedDevice) {
        let _ = self.shared.set(shared);
    }
}

impl EngineFactory for GpuEngineFactory {
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
        // Carte de l'interface inadaptée au calcul (OpenGL, par exemple) :
        // le moteur ouvre sa propre carte, sans affichage direct.
        let shared = self.shared.get().filter(|s| Gpu::supports(s));
        let engine = match shared {
            Some(shared) => GpuEngine::with_device(state, params, shared)?,
            None => GpuEngine::new(state, params)?,
        };
        Ok(Box::new(engine))
    }
}

// ----------------------------------------------------------------------
// Moteur
// ----------------------------------------------------------------------

struct Kernels {
    kick_drift: Kernel,
    kick: Kernel,
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
    query: Kernel,
}

impl Kernels {
    fn new(g: &Gpu) -> Self {
        macro_rules! k {
            ($name:literal) => {
                g.kernel($name, include_str!(concat!("shaders/", $name, ".wgsl")))
            };
        }
        Self {
            kick_drift: k!("kick_drift"),
            kick: k!("kick"),
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
            forces: k!("forces"),
            collide: k!("collide"),
            apply_merges: k!("apply_merges"),
            cleanup: k!("cleanup"),
            query: k!("query"),
        }
    }
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
    pairs: wgpu::Buffer,
    pair_counter: wgpu::Buffer,
    ops: wgpu::Buffer,
    removed: wgpu::Buffer,
    removed_counter: wgpu::Buffer,
    query_results: wgpu::Buffer,
    params: wgpu::Buffer,
}

/// Accès direct aux corps du moteur, pour les dessiner sans copie.
///
/// Le moteur alterne entre deux tampons (tri, compaction) : [`current`](Self::current)
/// désigne celui qui contient l'état publié en dernier. Comme le calcul et
/// l'affichage partagent la même file de commandes, un dessin soumis après
/// la lecture voit toujours un ensemble complet de corps. Seule exception,
/// sans conséquence : pendant une image, un pas qui élimine des corps peut
/// faire apparaître quelques doublons.
///
/// Chaque corps occupe [`STRIDE`](Self::STRIDE) octets ; position (3 × f32)
/// et masse (f32) sont aux décalages [`POSITION_OFFSET`](Self::POSITION_OFFSET)
/// et [`MASS_OFFSET`](Self::MASS_OFFSET). Un corps de masse nulle est mort.
pub struct BodiesView {
    buffers: [wgpu::Buffer; 2],
    /// Indice du tampon courant (bit 32) et nombre de corps (32 bits bas).
    current: AtomicU64,
}

impl BodiesView {
    pub const STRIDE: u64 = std::mem::size_of::<GpuBody>() as u64;
    pub const POSITION_OFFSET: u64 = 0;
    pub const MASS_OFFSET: u64 = 12;

    /// Tampon courant et nombre de corps qu'il contient.
    pub fn current(&self) -> (&wgpu::Buffer, u32) {
        let v = self.current.load(Ordering::Acquire);
        (&self.buffers[(v >> 32) as usize & 1], v as u32)
    }

    fn publish(&self, cur: usize, n: usize) {
        self.current
            .store(((cur as u64) << 32) | n as u64, Ordering::Release);
    }
}

/// Moteur Barnes-Hut sur GPU.
pub struct GpuEngine {
    gpu: Gpu,
    k: Kernels,
    b: Buffers,
    params: PhysicsParams,
    capacity: usize,
    pair_cap: usize,
    /// Indice du tampon de corps courant (double tampon).
    cur: usize,
    n: usize,
    step: u64,
    time: f64,
    /// Tampons exposés à l'affichage (carte partagée uniquement).
    view: Option<Arc<BodiesView>>,
    /// Moments globaux du dernier pas (évite de les recalculer quand
    /// énergies et centre de masse sont demandés ensemble).
    moments: Cell<Option<(u64, [[f32; 4]; 3])>>,
}

impl GpuEngine {
    /// Moteur sur sa propre carte graphique (banc d'essai, tests, sans fenêtre).
    pub fn new(state: SimulationState, params: PhysicsParams) -> Result<Self, String> {
        Self::build(Gpu::open()?, state, params, false)
    }

    /// Moteur sur la carte graphique de l'interface : ses corps peuvent être
    /// dessinés directement ([`PhysicsEngine::live_view`]).
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
        if state.bodies.ids.iter().any(|&id| id > u32::MAX as u64) {
            return Err("identifiants de corps trop grands pour le moteur GPU".to_owned());
        }
        let capacity = n.max(2);
        let body_bytes = (capacity * std::mem::size_of::<GpuBody>()) as u64;
        let node_bytes = (capacity * 64) as u64;
        let largest = body_bytes.max(node_bytes);
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

        // Espace de travail du scan : la plus grande des deux utilisations
        // (histogrammes du tri : 16 × groupes ; drapeaux de compaction : n).
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
            pairs: gpu.storage(
                "pairs",
                pair_cap as u64 * std::mem::size_of::<PairRec>() as u64,
            ),
            pair_counter: gpu.storage("pair-counter", 16),
            ops: gpu.storage(
                "ops",
                2 * pair_cap as u64 * std::mem::size_of::<MergeOp>() as u64,
            ),
            removed: gpu.storage("removed", cap * 4),
            removed_counter: gpu.storage("removed-counter", 16),
            query_results: gpu.storage("query-results", 16 * 16),
            params: gpu.uniform("params", bytemuck::bytes_of(&Params::default())),
        };

        // Téléversement de l'état initial.
        let bodies: Vec<GpuBody> = (0..n)
            .map(|i| {
                let s = &state.bodies;
                GpuBody {
                    pos: f3(s.positions[i]),
                    mass: s.masses[i] as f32,
                    vel: f3(s.velocities[i]),
                    id: s.ids[i] as u32,
                    old: f3(s.positions[i]),
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
            params,
            capacity,
            pair_cap,
            cur: 0,
            n,
            step: state.step,
            time: state.time,
            view,
            moments: Cell::new(None),
        };
        engine.write_params(0);
        let mut enc = engine.gpu.encoder();
        engine.encode_tree_and_forces(&mut enc);
        engine.gpu.submit(enc);
        engine.publish_view();
        Ok(engine)
    }

    /// Désigne à l'affichage le tampon courant (après soumission des travaux).
    fn publish_view(&self) {
        if let Some(v) = &self.view {
            v.publish(self.cur, self.n);
        }
    }

    /// Nom de la carte graphique utilisée.
    pub fn adapter_name(&self) -> &str {
        &self.gpu.adapter_name
    }

    pub fn capacity(&self) -> usize {
        self.capacity
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
        };
        self.gpu
            .queue
            .write_buffer(&self.b.params, 0, bytemuck::bytes_of(&params));
    }

    fn pass_uniform(&self, pass: Pass) -> wgpu::Buffer {
        self.gpu.uniform("pass", bytemuck::bytes_of(&pass))
    }

    /// Scan exclusif en place de `len` entiers de `data` (récursif).
    fn encode_scan(
        &self,
        enc: &mut wgpu::CommandEncoder,
        data: &wgpu::Buffer,
        len: usize,
        level: usize,
    ) {
        let wg = groups(len);
        let u = self.pass_uniform(Pass {
            n: len as u32,
            ..Default::default()
        });
        let sums = &self.b.scan_sums[level];
        self.gpu
            .run(enc, &self.k.scan_blocks, &[&u, data, sums], wg);
        if wg > 1 {
            self.encode_scan(enc, sums, wg as usize, level + 1);
            self.gpu.run(enc, &self.k.scan_add, &[&u, data, sums], wg);
        }
    }

    /// Tri radix (clé 30 bits, 4 bits par passe) de keys[0]/vals[0].
    fn encode_sort(&self, enc: &mut wgpu::CommandEncoder) {
        let n = self.n;
        let wg = groups(n);
        let b = &self.b;
        let mut src = 0;
        for pass in 0..8u32 {
            let u = self.pass_uniform(Pass {
                n: n as u32,
                a: 4 * pass,
                b: wg,
                c: 0,
            });
            let dst = 1 - src;
            self.gpu
                .run(enc, &self.k.radix_hist, &[&u, &b.keys[src], &b.counts], wg);
            self.encode_scan(enc, &b.counts, 16 * wg as usize, 0);
            self.gpu.run(
                enc,
                &self.k.radix_scatter,
                &[
                    &u,
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

    /// Réordonne les corps (Morton), construit l'arbre et calcule le champ.
    fn encode_tree_and_forces(&mut self, enc: &mut wgpu::CommandEncoder) {
        let n = self.n;
        if n == 0 {
            return;
        }
        let wg = groups(n);
        let mut cur = self.cur;
        if n >= 2 {
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
            let bodies = &b.bodies[cur];
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
                    ],
                    &b.level_args,
                    level as u64 * 12,
                );
            }
        }
        let b = &self.b;
        self.gpu.run(
            enc,
            &self.k.forces,
            &[&b.params, &b.bodies[cur], &b.nodes, &b.children],
            wg,
        );
        self.cur = cur;
    }

    /// Moments globaux (masse, centre de masse, quantité de mouvement, énergies).
    fn encode_moments(&self, enc: &mut wgpu::CommandEncoder) {
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

    fn read_moments(&self) -> [[f32; 4]; 3] {
        if let Some((step, m)) = self.moments.get() {
            if step == self.step {
                return m;
            }
        }
        let mut enc = self.gpu.encoder();
        self.encode_moments(&mut enc);
        self.gpu.submit(enc);
        let raw = self.gpu.read(&self.b.moments, 0, 48);
        let f: &[f32] = bytemuck::cast_slice(&raw);
        let m = [
            [f[0], f[1], f[2], f[3]],
            [f[4], f[5], f[6], f[7]],
            [f[8], f[9], f[10], f[11]],
        ];
        self.moments.set(Some((self.step, m)));
        m
    }

    /// Supprime les corps de masse nulle (compaction par scan).
    fn encode_compaction(&mut self, enc: &mut wgpu::CommandEncoder) {
        let b = &self.b;
        let wg = groups(self.n);
        // Les drapeaux réutilisent le tampon des histogrammes du tri.
        self.gpu.run(
            enc,
            &self.k.alive_flags,
            &[&b.params, &b.bodies[self.cur], &b.counts],
            wg,
        );
        self.encode_scan(enc, &b.counts, self.n, 0);
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

    /// Regroupe les paires en contact et prépare les opérations de fusion.
    fn plan_merges(&self, pairs: &[PairRec]) -> (Vec<MergeOp>, Vec<(BodyId, BodyId)>) {
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
            let a = slot(p.ia, p.a, &mut members);
            let b = slot(p.ib, p.b, &mut members);
            links.push((a, b));
        }
        // Union-find sur les indices locaux.
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
                            old: body.old,
                            flag: 0,
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
        (ops, merges)
    }

    fn step_inner(&mut self) -> StepReport {
        let mut report = StepReport::default();
        if self.n == 0 {
            self.step += 1;
            self.time += self.params.dt;
            return report;
        }
        let wg = groups(self.n);

        // 1. Kick ½, drift, arbre, champ, contacts.
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
                    &b.nodes,
                    &b.children,
                    &b.pairs,
                    &b.pair_counter,
                ],
                wg,
            );
        }
        self.gpu.submit(enc);

        // 2. Fusions (regroupement sur le CPU).
        let found = self.gpu.read_u32(&self.b.pair_counter) as usize;
        let mut dead = 0usize;
        let mut ops = Vec::new();
        if found > 0 {
            let count = found.min(self.pair_cap);
            let raw = self.gpu.read(
                &self.b.pairs,
                0,
                (count * std::mem::size_of::<PairRec>()) as u64,
            );
            let pairs: &[PairRec] = bytemuck::cast_slice(&raw);
            let (o, merges) = self.plan_merges(pairs);
            dead += merges.len();
            report.merges = merges;
            ops = o;
        }

        // 3. Application des fusions, kick ½, nettoyage, compaction.
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
        {
            let b = &self.b;
            self.gpu.run(
                &mut enc,
                &self.k.kick,
                &[&b.params, &b.bodies[self.cur]],
                wg,
            );
        }
        self.step += 1;
        self.time += self.params.dt;
        let period = self.params.cleanup_period as u64;
        if period > 0 && self.step % period == 0 {
            self.encode_moments(&mut enc);
            let b = &self.b;
            enc.clear_buffer(&b.removed_counter, 0, None);
            self.gpu.run(
                &mut enc,
                &self.k.cleanup,
                &[
                    &b.params,
                    &b.bodies[self.cur],
                    &b.moments,
                    &b.removed,
                    &b.removed_counter,
                ],
                wg,
            );
            self.gpu.submit(enc);
            let removed = self.gpu.read_u32(&self.b.removed_counter) as usize;
            if removed > 0 {
                let raw = self.gpu.read(&self.b.removed, 0, removed as u64 * 4);
                let ids: &[u32] = bytemuck::cast_slice(&raw);
                report.removed = ids.iter().map(|&i| i as u64).collect();
                dead += removed;
            }
            enc = self.gpu.encoder();
        }
        if dead > 0 {
            self.encode_compaction(&mut enc);
        }
        self.gpu.submit(enc);
        self.n -= dead.min(self.n);
        self.write_params(0);
        self.publish_view();
        report
    }

    /// Relit tous les corps (ordre interne du moteur).
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
        // Conversion directe depuis la mémoire projetée, en parallèle, dans
        // les tableaux existants de `out` (pas de copie intermédiaire).
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
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(15) {
            let mut q = [0u32; 16];
            q[0] = chunk.len() as u32;
            for (k, &id) in chunk.iter().enumerate() {
                q[k + 1] = id.min(u32::MAX as u64) as u32;
            }
            let qbuf = self.gpu.uniform("query", bytemuck::cast_slice(&q));
            let b = &self.b;
            let mut enc = self.gpu.encoder();
            enc.clear_buffer(&b.query_results, 0, None);
            self.gpu.run(
                &mut enc,
                &self.k.query,
                &[&b.params, &qbuf, &b.bodies[self.cur], &b.query_results],
                groups(self.n),
            );
            self.gpu.submit(enc);
            let raw = self.gpu.read(&b.query_results, 0, chunk.len() as u64 * 16);
            let r: &[[f32; 4]] = bytemuck::cast_slice(&raw);
            out.extend(
                r.iter().map(|v| {
                    (v[3] > 0.5).then(|| DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64))
                }),
            );
        }
        out
    }

    fn center_of_mass(&self) -> Option<DVec3> {
        if self.n == 0 {
            return None;
        }
        let m = self.read_moments();
        let mass = m[0][0] as f64;
        (mass > 0.0).then(|| DVec3::new(m[0][1] as f64, m[0][2] as f64, m[0][3] as f64) / mass)
    }

    fn diagnostics(&self) -> Diagnostics {
        if self.n == 0 {
            return Diagnostics::default();
        }
        let m = self.read_moments();
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
