//! # stellar-engine-cpu
//!
//! Moteur de calcul CPU :
//! - forces gravitationnelles par **Barnes-Hut** (octree de Morton, critère
//!   d'ouverture θ, adoucissement de Plummer ε), parallélisées avec `rayon` ;
//! - intégration **Leapfrog KDK** à pas fixe ;
//! - **fusions** sur contact balayé, regroupées par union-find ;
//! - **nettoyage** périodique des corps éjectés (non liés et lointains).
//!
//! Conformément à l'architecture retenue, ce module privilégie la
//! performance (structure de tableaux, réordonnancement de Morton à chaque
//! pas, parcours d'arbre sans pile) plutôt que l'abstraction.

pub mod collisions;
pub mod morton;
pub mod octree;

use std::collections::HashMap;
use std::sync::Arc;

use glam::DVec3;
use rayon::prelude::*;
use stellar_domain::physics::{body_radius, merge, MergeInput};
use stellar_domain::{
    BodyId, BodySet, Diagnostics, EngineDescriptor, EngineFactory, PhysicsEngine, PhysicsParams,
    SimulationState, StepReport,
};

use crate::collisions::{find_contacts, DisjointSets};
use crate::morton::Cube;
use crate::octree::Node;

pub const ENGINE_ID: &str = "cpu-barnes-hut";

/// Fabrique du moteur CPU.
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuEngineFactory;

impl EngineFactory for CpuEngineFactory {
    fn descriptor(&self) -> EngineDescriptor {
        EngineDescriptor {
            id: ENGINE_ID,
            label: "CPU — Barnes-Hut multithread",
            description:
                "Octree de Barnes-Hut sur tous les cœurs (sauf un, réservé à l'affichage).",
        }
    }

    fn create(
        &self,
        state: SimulationState,
        params: PhysicsParams,
    ) -> Result<Box<dyn PhysicsEngine>, String> {
        Ok(Box::new(CpuEngine::new(state, params)))
    }
}

/// Moteur Barnes-Hut sur CPU.
pub struct CpuEngine {
    params: PhysicsParams,
    pool: Arc<rayon::ThreadPool>,
    ids: Vec<BodyId>,
    pos: Vec<DVec3>,
    vel: Vec<DVec3>,
    mass: Vec<f64>,
    acc: Vec<DVec3>,
    /// Potentiel gravitationnel ressenti par chaque corps (G inclus).
    pot: Vec<f64>,
    old_pos: Vec<DVec3>,
    nodes: Vec<Node>,
    step: u64,
    time: f64,
    diagnostics: Diagnostics,
}

impl CpuEngine {
    pub fn new(state: SimulationState, mut params: PhysicsParams) -> Self {
        // θ ≤ 1 garantit qu'un corps n'est jamais approximé par un nœud qui le contient.
        params.theta = params.theta.clamp(0.0, 1.0);
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(1);
        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("stellar-cpu-{i}"))
                .build()
                .expect("impossible de créer le pool de calcul"),
        );
        let BodySet {
            ids,
            positions,
            velocities,
            masses,
        } = state.bodies;
        let n = ids.len();
        let mut engine = Self {
            params,
            pool,
            ids,
            pos: positions,
            vel: velocities,
            mass: masses,
            acc: vec![DVec3::ZERO; n],
            pot: vec![0.0; n],
            old_pos: Vec::new(),
            nodes: Vec::new(),
            step: state.step,
            time: state.time,
            diagnostics: Diagnostics::default(),
        };
        engine.pool_install(|e| e.compute_forces());
        engine
    }

    pub fn params(&self) -> &PhysicsParams {
        &self.params
    }

    /// Identifiants et accélérations courants (ordre interne du moteur).
    pub fn accelerations(&self) -> (&[BodyId], &[DVec3]) {
        (&self.ids, &self.acc)
    }

    /// Identifiants et potentiels courants (ordre interne du moteur).
    pub fn potentials(&self) -> (&[BodyId], &[f64]) {
        (&self.ids, &self.pot)
    }

    /// Exécute `f` dans le pool de calcul dédié.
    fn pool_install<R: Send>(&mut self, f: impl FnOnce(&mut Self) -> R + Send) -> R {
        let pool = Arc::clone(&self.pool);
        pool.install(|| f(self))
    }

    /// Réordonne les corps selon leur code de Morton, construit l'octree et
    /// calcule accélérations, potentiels et énergies.
    fn compute_forces(&mut self) {
        let n = self.pos.len();
        self.acc.resize(n, DVec3::ZERO);
        self.pot.resize(n, 0.0);
        if n == 0 {
            self.nodes.clear();
            self.diagnostics = Diagnostics::default();
            return;
        }

        // 1. Tri de Morton (localité spatiale ⇒ localité mémoire).
        let cube = Cube::bounding(&self.pos);
        let mut keyed: Vec<(u64, u32)> = self
            .pos
            .par_iter()
            .enumerate()
            .map(|(i, &p)| (cube.code(p), i as u32))
            .collect();
        keyed.par_sort_unstable_by_key(|k| k.0);
        let gather = |src: &Vec<DVec3>| -> Vec<DVec3> {
            keyed.par_iter().map(|&(_, i)| src[i as usize]).collect()
        };
        self.pos = gather(&self.pos);
        self.vel = gather(&self.vel);
        self.mass = keyed
            .par_iter()
            .map(|&(_, i)| self.mass[i as usize])
            .collect();
        self.ids = keyed
            .par_iter()
            .map(|&(_, i)| self.ids[i as usize])
            .collect();
        let codes: Vec<u64> = keyed.into_iter().map(|k| k.0).collect();

        // 2. Octree.
        self.nodes = octree::build(&codes, &self.pos, &self.mass, cube, self.params.theta);

        // 3. Champ en chaque corps, par groupes de corps voisins. Les
        //    groupes, en ordre préfixe, couvrent les corps dans l'ordre :
        //    leurs plages de sortie se découpent sans recouvrement.
        let eps2 = self.params.softening * self.params.softening;
        let g = self.params.g;
        let (nodes, pos, mass) = (&self.nodes, &self.pos, &self.mass);
        let mut work = Vec::new();
        let (mut acc, mut pot) = (&mut self.acc[..], &mut self.pot[..]);
        for group in octree::groups(nodes) {
            let count = group.count as usize;
            let (a, rest_a) = std::mem::take(&mut acc).split_at_mut(count);
            let (p, rest_p) = std::mem::take(&mut pot).split_at_mut(count);
            acc = rest_a;
            pot = rest_p;
            work.push((group, a, p));
        }
        work.into_par_iter().for_each_init(
            octree::Interactions::default,
            |list, (group, a, p)| {
                octree::field_of_group(nodes, pos, mass, group, eps2, list, (&mut *a, &mut *p));
                for v in a.iter_mut() {
                    *v *= g;
                }
                for v in p.iter_mut() {
                    *v *= g;
                }
            },
        );

        self.update_diagnostics();
    }

    fn update_diagnostics(&mut self) {
        let (kin, pot) = self
            .mass
            .par_iter()
            .zip(self.vel.par_iter())
            .zip(self.pot.par_iter())
            .map(|((&m, v), &phi)| (0.5 * m * v.length_squared(), 0.5 * m * phi))
            .reduce(|| (0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
        self.diagnostics = Diagnostics {
            kinetic_energy: kin,
            potential_energy: pot,
        };
    }

    /// Fusionne les corps entrés en contact durant la phase de dérive.
    fn resolve_collisions(&mut self) -> Vec<(BodyId, BodyId)> {
        let r0 = self.params.base_radius;
        if r0 <= 0.0 || self.pos.len() < 2 {
            return Vec::new();
        }
        let radius: Vec<f64> = self.mass.par_iter().map(|&m| body_radius(m, r0)).collect();
        let pairs = find_contacts(&self.old_pos, &self.pos, &radius);
        if pairs.is_empty() {
            return Vec::new();
        }

        let n = self.pos.len();
        let mut sets = DisjointSets::new(n);
        for &(a, b) in &pairs {
            sets.union(a, b);
        }
        let mut groups: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(a, b) in &pairs {
            for x in [a, b] {
                let root = sets.find(x);
                groups.entry(root).or_default().push(x);
            }
        }

        let mut alive = vec![true; n];
        let mut merges = Vec::new();
        for (_, mut members) in groups {
            members.sort_unstable();
            members.dedup();
            let inputs: Vec<MergeInput> = members
                .iter()
                .map(|&k| {
                    let k = k as usize;
                    MergeInput {
                        id: self.ids[k],
                        mass: self.mass[k],
                        position: self.pos[k],
                        velocity: self.vel[k],
                    }
                })
                .collect();
            let merged = merge(&inputs);
            for &k in &members {
                let k = k as usize;
                if self.ids[k] == merged.id {
                    self.mass[k] = merged.mass;
                    self.pos[k] = merged.position;
                    self.vel[k] = merged.velocity;
                } else {
                    alive[k] = false;
                    merges.push((self.ids[k], merged.id));
                }
            }
        }
        self.retain(&alive);
        merges
    }

    /// Supprime les corps éjectés : non liés ET au-delà de k × R du barycentre.
    fn cleanup(&mut self) -> Vec<BodyId> {
        let n = self.pos.len();
        if n < 2 {
            return Vec::new();
        }
        let (m, mx, mv) = self
            .mass
            .par_iter()
            .zip(self.pos.par_iter())
            .zip(self.vel.par_iter())
            .map(|((&m, &x), &v)| (m, x * m, v * m))
            .reduce(
                || (0.0, DVec3::ZERO, DVec3::ZERO),
                |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2),
            );
        if m <= 0.0 {
            return Vec::new();
        }
        let (x_cm, v_cm) = (mx / m, mv / m);
        let limit = self.params.cleanup_distance_factor * self.params.reference_radius;
        let limit2 = limit * limit;
        let alive: Vec<bool> = (0..n)
            .into_par_iter()
            .map(|i| {
                let far = (self.pos[i] - x_cm).length_squared() > limit2;
                let unbound = 0.5 * (self.vel[i] - v_cm).length_squared() + self.pot[i] > 0.0;
                !(far && unbound)
            })
            .collect();
        let removed: Vec<BodyId> = (0..n).filter(|&i| !alive[i]).map(|i| self.ids[i]).collect();
        if !removed.is_empty() {
            self.retain(&alive);
            self.update_diagnostics();
        }
        removed
    }

    /// Compacte tous les tableaux en ne gardant que les corps vivants.
    fn retain(&mut self, alive: &[bool]) {
        fn keep<T: Copy>(v: &mut Vec<T>, alive: &[bool]) {
            let mut k = 0;
            v.retain(|_| {
                let a = alive[k];
                k += 1;
                a
            });
        }
        keep(&mut self.ids, alive);
        keep(&mut self.pos, alive);
        keep(&mut self.vel, alive);
        keep(&mut self.mass, alive);
        if self.acc.len() == alive.len() {
            keep(&mut self.acc, alive);
        }
        if self.pot.len() == alive.len() {
            keep(&mut self.pot, alive);
        }
        if self.old_pos.len() == alive.len() {
            keep(&mut self.old_pos, alive);
        }
    }

    fn step_inner(&mut self) -> StepReport {
        let dt = self.params.dt;
        let half = 0.5 * dt;

        // Kick (½ dt)
        self.vel
            .par_iter_mut()
            .zip(self.acc.par_iter())
            .for_each(|(v, a)| *v += *a * half);

        // Drift (dt), en mémorisant les positions de départ pour le balayage.
        self.old_pos.clear();
        self.old_pos.extend_from_slice(&self.pos);
        self.pos
            .par_iter_mut()
            .zip(self.vel.par_iter())
            .for_each(|(x, v)| *x += *v * dt);

        // Contacts et fusions
        let merges = self.resolve_collisions();

        // Forces au nouvel instant, puis Kick (½ dt)
        self.compute_forces();
        self.vel
            .par_iter_mut()
            .zip(self.acc.par_iter())
            .for_each(|(v, a)| *v += *a * half);
        self.update_diagnostics();

        self.step += 1;
        self.time += dt;

        let period = self.params.cleanup_period as u64;
        let removed = if period > 0 && self.step.is_multiple_of(period) {
            self.cleanup()
        } else {
            Vec::new()
        };

        StepReport { merges, removed }
    }
}

impl PhysicsEngine for CpuEngine {
    fn step(&mut self) -> StepReport {
        self.pool_install(|e| e.step_inner())
    }

    fn state(&self) -> SimulationState {
        let mut s = SimulationState::default();
        self.write_state(&mut s);
        s
    }

    fn write_state(&self, out: &mut SimulationState) {
        let b = &mut out.bodies;
        b.ids.clear();
        b.ids.extend_from_slice(&self.ids);
        b.positions.clear();
        b.positions.extend_from_slice(&self.pos);
        b.velocities.clear();
        b.velocities.extend_from_slice(&self.vel);
        b.masses.clear();
        b.masses.extend_from_slice(&self.mass);
        out.step = self.step;
        out.time = self.time;
    }

    fn body_count(&self) -> usize {
        self.ids.len()
    }

    fn center_of_mass(&self) -> Option<DVec3> {
        let (m, mx) = self
            .mass
            .par_iter()
            .zip(self.pos.par_iter())
            .map(|(&m, &x)| (m, x * m))
            .reduce(|| (0.0, DVec3::ZERO), |a, b| (a.0 + b.0, a.1 + b.1));
        (m > 0.0).then(|| mx / m)
    }

    fn positions_of(&self, ids: &[BodyId]) -> Vec<Option<DVec3>> {
        // Un seul passage, en s'arrêtant dès que tout est trouvé.
        let mut out = vec![None; ids.len()];
        let mut remaining = ids.len();
        for (i, id) in self.ids.iter().enumerate() {
            if remaining == 0 {
                break;
            }
            for (k, w) in ids.iter().enumerate() {
                if w == id && out[k].is_none() {
                    out[k] = Some(self.pos[i]);
                    remaining -= 1;
                }
            }
        }
        out
    }

    fn diagnostics(&self) -> Diagnostics {
        self.diagnostics
    }
}
