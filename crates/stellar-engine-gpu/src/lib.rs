//! # stellar-engine-gpu
//!
//! Moteur de calcul sur carte graphique (shaders de calcul wgpu : Vulkan,
//! DirectX 12, Metal), même algorithme que le moteur CPU :
//!
//! - **Barnes-Hut sur LBVH** : à chaque pas, boîte englobante, codes de
//!   Morton (30 bits), tri radix GPU, construction parallèle de l'arbre
//!   binaire radix (Karras 2012), agrégation niveau par niveau (centres de
//!   masse, boîtes, rayons d'ouverture, boîtes balayées), puis parcours
//!   **par groupes** de 128 corps voisins, qui partagent la descente de
//!   l'arbre et une liste d'interactions en mémoire partagée ;
//! - **Leapfrog KDK** à pas fixe ;
//! - **fusions balayées** : les paires en contact sont détectées sur le GPU
//!   (même arbre, boîtes balayées) ; le regroupement (union-find, rarement
//!   plus de quelques milliers de paires) est fait sur le CPU, puis appliqué
//!   sur le GPU ;
//! - **nettoyage** des corps éjectés et compaction sur le GPU.
//!
//! Un pas ne coûte qu'**une soumission et une attente** : tout ce que
//! l'application lit ensuite (paires de contact, éliminés, moments globaux,
//! positions des corps suivis) est recopié en une fois dans un tampon de
//! relecture. Les fusions planifiées sont appliquées dans une seconde
//! soumission, sans attente. Les accesseurs ([`PhysicsEngine::positions_of`],
//! [`PhysicsEngine::center_of_mass`], [`PhysicsEngine::diagnostics`])
//! répondent alors sans solliciter la carte.
//!
//! Les calculs sont en simple précision (f32) ; l'état est relu en double
//! précision quand l'application le demande (sauvegarde, enregistrement…).
//!
//! Construit sur la carte graphique de l'interface ([`GpuEngineFactory::attach`]),
//! le moteur expose ses tampons de corps ([`BodiesView`]) : l'affichage les
//! lit directement, sans aller-retour par la mémoire centrale.

mod gpu;

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

use crate::gpu::{grid, groups, Gpu, Kernel};

pub use crate::gpu::{SharedDevice, WG};

pub const ENGINE_ID: &str = "gpu-barnes-hut";

/// Nombre maximal de niveaux de l'arbre (clés 30 bits + départage 32 bits).
const LEVELS: usize = 72;
/// Nombre maximal de paires de contact traitées par pas.
const MAX_PAIRS: usize = 1 << 18;
/// Taille d'un nœud de l'arbre (centre de masse, boîte, rayon d'ouverture).
const NODE_BYTES: usize = 48;
/// La topologie de l'arbre (ordre de Morton, construction de Karras) est
/// refaite tous les `REBUILD_EVERY` pas, ou après une compaction ; entre-temps
/// l'arbre est seulement réajusté (centres de masse, boîtes, rayons
/// d'ouverture), les corps ayant peu bougé.
const REBUILD_EVERY: u32 = 4;
/// Corps par groupe du parcours de Barnes-Hut (`forces.wgsl`).
const FORCE_GROUP: u32 = 128;
/// Nombre maximal de corps (lancements 1D : 65 535 groupes de 256).
pub const MAX_BODIES: usize = 65_535 * WG as usize;
/// Identifiants dont la position est relue à chaque pas (traces, suivi).
const QUERY_CAP: usize = 1024;
/// Listes d'identifiants suivies à chaque pas (les plus récemment demandées).
const QUERY_LISTS: usize = 4;
/// Paires de contact et éliminés relus avec chaque pas ; au-delà (rare),
/// une relecture complémentaire.
const PAIR_PREFIX: usize = 1024;
const REMOVED_PREFIX: usize = 1024;

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

/// Disposition du tampon de relecture d'un pas (octets).
mod readback {
    use super::*;
    /// Nombre de paires de contact, nombre d'éliminés (u32).
    pub const COUNTERS: u64 = 0;
    /// Moments globaux : 3 × vec4<f32>.
    pub const MOMENTS: u64 = 16;
    /// Positions des corps suivis : QUERY_CAP × vec4<f32>.
    pub const QUERY: u64 = 64;
    pub const PAIRS: u64 = QUERY + 16 * QUERY_CAP as u64;
    pub const REMOVED: u64 = PAIRS + (PAIR_PREFIX * PAIR_BYTES) as u64;
    pub const SIZE: u64 = REMOVED + 4 * REMOVED_PREFIX as u64;
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
        // Parcours de Barnes-Hut : liste d'interactions de 512 entrées, pile
        // aussi grande que la mémoire partagée le permet (2 048 nœuds au
        // plus, pour garder plusieurs groupes actifs par multiprocesseur).
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

/// Paramètres de passe (tri, scan, requêtes) : un petit tampon uniforme par
/// usage, réécrit seulement quand sa valeur change. Les tampons restant les
/// mêmes, leurs groupes de liaisons sont créés une fois pour toutes.
///
/// Un même emplacement ne doit servir qu'avec une seule valeur par
/// soumission (les écritures de la file précèdent toute la soumission).
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

/// Emplacements des paramètres de passe.
mod slot {
    /// Huit passes du tri radix.
    pub const SORT: usize = 0;
    /// Niveaux du scan des histogrammes du tri.
    pub const SORT_SCAN: usize = 8;
    // Puis les niveaux du scan de la compaction, et enfin les requêtes.
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
    removed: wgpu::Buffer,
    removed_counter: wgpu::Buffer,
    index_of: wgpu::Buffer,
    query_ids: wgpu::Buffer,
    query_results: wgpu::Buffer,
    params: wgpu::Buffer,
    /// Tampon de relecture du pas (disposition : module [`readback`]).
    readback: wgpu::Buffer,
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

/// Listes d'identifiants dont les positions sont relues à chaque pas.
#[derive(Default)]
struct Queries {
    /// Listes suivies, la plus récemment demandée en dernier.
    lists: Vec<Vec<BodyId>>,
    /// Positions relues, dans l'ordre de concaténation des listes.
    results: Vec<Option<DVec3>>,
    /// Pas auquel `results` correspond.
    step: Option<u64>,
}

impl Queries {
    fn total(&self) -> usize {
        self.lists.iter().map(Vec::len).sum()
    }

    /// Positions de la liste `ids` si elle est suivie et à jour.
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

/// Fusions d'un pas : opérations à appliquer sur le GPU, paires (absorbé,
/// résultant) et position de chaque corps résultant.
struct MergePlan {
    ops: Vec<MergeOp>,
    merges: Vec<(BodyId, BodyId)>,
    survivors: Vec<(BodyId, DVec3)>,
}

/// Grandeurs relues à l'issue d'un pas.
#[derive(Clone, Copy, Default)]
struct Moments {
    /// (m, m·x), (m·v, ½ m v²), (½ m φ, 0, 0, 0), sommés sur tous les corps.
    raw: [[f32; 4]; 3],
}

/// Moteur Barnes-Hut sur GPU.
pub struct GpuEngine {
    gpu: Gpu,
    k: Kernels,
    b: Buffers,
    slots: PassSlots,
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
    moments: Moments,
    queries: RefCell<Queries>,
    /// La table des indices est périmée (compaction depuis le dernier arbre).
    map_stale: std::cell::Cell<bool>,
    /// Pas écoulés depuis la dernière construction de la topologie de
    /// l'arbre (`u32::MAX` : à refaire).
    tree_age: u32,
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
        let max_id = state.bodies.ids.iter().copied().max().unwrap_or(0);
        if max_id >= u32::MAX as u64 {
            return Err("identifiants de corps trop grands pour le moteur GPU".to_owned());
        }
        let capacity = n.max(2);
        let body_bytes = (capacity * std::mem::size_of::<GpuBody>()) as u64;
        let node_bytes = (capacity * NODE_BYTES) as u64;
        let largest = body_bytes.max(node_bytes).max((max_id + 1) * 4);
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
            ops: gpu.storage(
                "ops",
                2 * pair_cap as u64 * std::mem::size_of::<MergeOp>() as u64,
            ),
            removed: gpu.storage("removed", cap * 4),
            removed_counter: gpu.storage("removed-counter", 16),
            index_of: gpu.storage("index-of", (max_id + 1) * 4),
            query_ids: gpu.storage("query-ids", QUERY_CAP as u64 * 4),
            query_results: gpu.storage("query-results", QUERY_CAP as u64 * 16),
            params: gpu.uniform("params", bytemuck::bytes_of(&Params::default())),
            readback: gpu.readback("readback", readback::SIZE),
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
        // Premier calcul du champ (le premier kick en a besoin) et des moments.
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

    /// Temps GPU cumulés par noyau (ms, lancements), si le profilage est
    /// actif (`STELLAR_GPU_PROFILE=1`, carte propre au moteur).
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
        };
        self.gpu
            .queue
            .write_buffer(&self.b.params, 0, bytemuck::bytes_of(&params));
    }

    /// Scan exclusif en place de `len` entiers de `data` (récursif) ;
    /// `slot_base` : emplacements des paramètres, un par niveau.
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

    /// Tri radix (clé 30 bits, 4 bits par passe) de keys[0]/vals[0].
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

    /// Réordonne les corps (Morton) et construit l'arbre, ou le réajuste
    /// seulement (voir [`REBUILD_EVERY`]), puis calcule le champ et tient à
    /// jour la table des indices.
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

    /// Moments globaux (masse, centre de masse, quantité de mouvement, énergies).
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

    /// Positions des listes suivies (au plus `QUERY_CAP` identifiants,
    /// déjà écrits dans `query_ids`).
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

    /// Supprime les corps de masse nulle (compaction par scan).
    fn encode_compaction(&mut self, enc: &mut gpu::Encoder) {
        let b = &self.b;
        let wg = groups(self.n);
        // Les drapeaux réutilisent le tampon des histogrammes du tri.
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

    /// Regroupe les paires en contact et prépare les opérations de fusion.
    ///
    /// Les corps éliminés au même pas par le nettoyage sont écartés des
    /// paires (ils ont déjà disparu). Renvoie aussi, pour chaque groupe,
    /// l'identifiant du corps résultant et sa position.
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

        // 1. Kick ½, drift, arbre, champ, contacts, kick ½, nettoyage,
        //    moments et positions suivies : une soumission, une attente.
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
            // Kick ½ et moments globaux en une lecture des corps.
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
        enc.clear_buffer(&self.b.removed_counter, 0, None);
        if cleanup {
            let b = &self.b;
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
            // Moments sans les corps éliminés.
            self.encode_moments(&mut enc);
        }
        let query_count = self.queries.borrow().total();
        self.encode_queries(&mut enc, query_count);
        let index = {
            let b = &self.b;
            let rb = &b.readback;
            enc.copy_buffer_to_buffer(&b.pair_counter, 0, rb, readback::COUNTERS, 4);
            enc.copy_buffer_to_buffer(&b.removed_counter, 0, rb, readback::COUNTERS + 4, 4);
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
                let removed = 4 * REMOVED_PREFIX.min(self.capacity) as u64;
                enc.copy_buffer_to_buffer(&b.removed, 0, rb, readback::REMOVED, removed);
            }
            self.gpu.submit(enc)
        };
        self.publish_view();

        // 2. Relecture : compteurs, moments, positions, premières paires.
        let mut found = 0usize;
        let mut removed_count = 0usize;
        let mut pairs: Vec<PairRec> = Vec::new();
        let mut removed_ids: Vec<u32> = Vec::new();
        let mut query_raw: Vec<[f32; 4]> = Vec::new();
        let mut moments = [[0f32; 4]; 3];
        self.gpu
            .map_read(&self.b.readback, index, readback::SIZE, |raw| {
                found = u32_at(raw, readback::COUNTERS as usize) as usize;
                removed_count = u32_at(raw, readback::COUNTERS as usize + 4) as usize;
                moments = read_moments(raw);
                let q = readback::QUERY as usize;
                query_raw = bytemuck::cast_slice(&raw[q..q + 16 * query_count]).to_vec();
                let count = found.min(self.pair_cap).min(PAIR_PREFIX);
                let p = readback::PAIRS as usize;
                pairs = bytemuck::cast_slice(&raw[p..p + count * PAIR_BYTES]).to_vec();
                if cleanup {
                    let count = removed_count.min(REMOVED_PREFIX);
                    let r = readback::REMOVED as usize;
                    removed_ids = bytemuck::cast_slice(&raw[r..r + 4 * count]).to_vec();
                }
            });
        // Au-delà des premiers enregistrements (rare) : relecture du reste.
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
        if removed_count > removed_ids.len() {
            let start = removed_ids.len();
            let raw = self.gpu.read(
                &self.b.removed,
                start as u64 * 4,
                (removed_count - start) as u64 * 4,
            );
            removed_ids.extend_from_slice(bytemuck::cast_slice(&raw));
        }
        report.removed = removed_ids.iter().map(|&i| i as u64).collect();
        self.moments = Moments { raw: moments };

        // 3. Fusions : regroupement sur le CPU, application sans attente.
        let mut dead = report.removed.len();
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
        if dead > 0 {
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
            // Les paramètres suivants ne valent qu'à partir de la prochaine
            // soumission, une fois la compaction faite.
            self.n -= dead.min(self.n);
            self.write_params(0);
            self.map_stale.set(true);
            self.tree_age = u32::MAX;
            self.publish_view();
        }

        // 4. Positions suivies, corrigées des fusions et éliminations du pas.
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
        report
    }

    /// Relit sur-le-champ les positions de toutes les listes suivies.
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
        if let Some(r) = self.queries.borrow().cached(ids, self.step) {
            return r;
        }
        if ids.is_empty() {
            return Vec::new();
        }
        if ids.len() > QUERY_CAP {
            // Liste trop longue pour être suivie : relecture par l'état.
            let s = self.state();
            let index: HashMap<BodyId, usize> =
                s.bodies.ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
            return ids
                .iter()
                .map(|id| index.get(id).map(|&i| s.bodies.positions[i]))
                .collect();
        }
        // Nouvelle liste : suivie désormais à chaque pas (à la place de la
        // plus ancienne), et relue tout de suite.
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
            .expect("liste suivie")
    }

    fn center_of_mass(&self) -> Option<DVec3> {
        let m = self.moments.raw;
        let mass = m[0][0] as f64;
        (self.n > 0 && mass > 0.0)
            .then(|| DVec3::new(m[0][1] as f64, m[0][2] as f64, m[0][3] as f64) / mass)
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
