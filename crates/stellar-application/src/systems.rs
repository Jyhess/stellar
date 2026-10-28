//! Moteur composite : une simulation découpée en sous-systèmes.
//!
//! Chaque sous-système est calculé par son propre moteur (créé par la même
//! fabrique), dans son propre repère : positions relatives à une origine
//! proche de son barycentre, ce qui préserve la précision d'un amas parti
//! très loin. Les sous-systèmes avancent au même pas ; chacun ressent les
//! autres par un champ linéaire (voir [`stellar_domain::systems`]), calculé
//! sur les barycentres extrapolés à la fin du pas.
//!
//! À chaque nettoyage :
//! - un amas éjecté assez éloigné de son système devient un sous-système ;
//! - deux sous-systèmes trop proches sont réunis ;
//! - un sous-système dont le barycentre s'est écarté de son origine est
//!   recentré ;
//! - les corps qu'un sous-système secondaire élimine (évadés) ne sont pas
//!   perdus : ils rejoignent le système principal (le plus massif), dont le
//!   nettoyage décidera de leur sort.
//!
//! Le reste de l'application voit un moteur ordinaire ; avec un seul
//! sous-système (cas courant), le composite se contente de relayer.

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use stellar_domain::systems::{ViewPart, MERGE_RATIO, SPLIT_RATIO};
use stellar_domain::{
    ranking, BodyId, BodySet, Diagnostics, EjectedCluster, EngineFactory, ExternalField, Moments,
    PhysicsEngine, PhysicsParams, SimulationState, StepReport, SystemSpan, SystemSummary,
    SystemsView, Vec3,
};

struct Subsystem {
    engine: Box<dyn PhysicsEngine>,
    /// Origine du repère du moteur (coordonnées globales).
    origin: Vec3,
    size: f64,
    anchor: BodyId,
    /// Moments au dernier pas (coordonnées globales).
    moments: Moments,
}

impl Subsystem {
    fn summary(&self) -> SystemSummary {
        SystemSummary {
            anchor: self.anchor,
            bodies: self.engine.body_count(),
            mass: self.moments.mass,
            center: self.moments.center,
            velocity: self.moments.velocity,
            size: self.size,
        }
    }
}

/// Moteur composite (voir le module).
pub struct MultiSystem {
    factory: Arc<dyn EngineFactory>,
    params: PhysicsParams,
    initial_mass: Option<f64>,
    systems: Vec<Subsystem>,
    /// Affichage direct composite, si les moteurs en offrent un.
    view: Option<Arc<SystemsView>>,
}

fn shift(bodies: &mut BodySet, by: Vec3) {
    for p in &mut bodies.positions {
        *p += by;
    }
}

fn heaviest(bodies: &BodySet) -> BodyId {
    ranking::heaviest(&bodies.ids, &bodies.masses)
        .map(|i| bodies.ids[i])
        .unwrap_or_default()
}

fn moments_of(bodies: &BodySet) -> Moments {
    let mass = bodies.total_mass();
    Moments {
        mass,
        center: bodies.center_of_mass().unwrap_or_default(),
        velocity: if mass > 0.0 {
            bodies.momentum() / mass
        } else {
            Vec3::ZERO
        },
    }
}

impl MultiSystem {
    /// Construit les sous-systèmes décrits par `state.systems` (un seul si
    /// la liste est vide ; son repère est alors celui de l'état).
    pub fn new(
        factory: Arc<dyn EngineFactory>,
        state: SimulationState,
        params: PhysicsParams,
    ) -> Result<Self, String> {
        let mut this = Self {
            factory,
            params,
            initial_mass: state.initial_mass,
            systems: Vec::new(),
            view: None,
        };
        let (step, time) = (state.step, state.time);
        let spans = if state.systems.len() > 1 {
            state.systems.clone()
        } else {
            vec![SystemSpan {
                start: 0,
                len: state.bodies.len(),
                size: params.reference_radius,
            }]
        };
        let single = spans.len() == 1;
        for span in spans {
            let b = &state.bodies;
            let range = span.start..(span.start + span.len).min(b.len());
            let bodies = BodySet {
                ids: b.ids[range.clone()].to_vec(),
                positions: b.positions[range.clone()].to_vec(),
                velocities: b.velocities[range.clone()].to_vec(),
                masses: b.masses[range].to_vec(),
            };
            let origin = if single {
                Vec3::ZERO
            } else {
                bodies.center_of_mass().unwrap_or_default()
            };
            let system = this.build(bodies, origin, span.size, step, time)?;
            this.systems.push(system);
        }
        if this.systems[0].engine.live_view().is_some() {
            this.view = Some(Arc::default());
            this.publish_view();
        }
        Ok(this)
    }

    /// Crée le moteur d'un sous-système à partir de corps en coordonnées
    /// globales.
    fn build(
        &self,
        mut bodies: BodySet,
        origin: Vec3,
        size: f64,
        step: u64,
        time: f64,
    ) -> Result<Subsystem, String> {
        let anchor = heaviest(&bodies);
        let moments = moments_of(&bodies);
        shift(&mut bodies, -origin);
        let state = SimulationState {
            bodies,
            step,
            time,
            initial_energy: None,
            initial_mass: self.initial_mass,
            systems: Vec::new(),
        };
        let mut params = self.params;
        params.reference_radius = size;
        let engine = self.factory.create(state, params)?;
        Ok(Subsystem {
            engine,
            origin,
            size,
            anchor,
            moments,
        })
    }

    /// Corps d'un sous-système, en coordonnées globales.
    fn bodies_of(system: &Subsystem) -> (BodySet, u64, f64) {
        let s = system.engine.state();
        let mut bodies = s.bodies;
        shift(&mut bodies, system.origin);
        (bodies, s.step, s.time)
    }

    /// Indice du système principal (le plus massif).
    fn root(&self) -> usize {
        (0..self.systems.len())
            .max_by(|&a, &b| {
                self.systems[a]
                    .moments
                    .mass
                    .total_cmp(&self.systems[b].moments.mass)
            })
            .unwrap_or(0)
    }

    fn publish_view(&self) {
        if let Some(view) = &self.view {
            view.set(
                self.systems
                    .iter()
                    .filter_map(|s| {
                        s.engine.live_view().map(|v| ViewPart {
                            view: v,
                            origin: s.origin,
                        })
                    })
                    .collect(),
            );
        }
    }

    /// Champs extérieurs valables à la fin du pas : barycentres extrapolés
    /// (vitesse et attraction mutuelle), sources ponctuelles.
    fn apply_fields(&mut self) {
        let g = self.params.g;
        let eps2 = self.params.softening * self.params.softening;
        let dt = self.params.dt;
        let m: Vec<Moments> = self.systems.iter().map(|s| s.moments).collect();
        let accel = |i: usize, at: Vec3, m: &[Moments]| -> Vec3 {
            m.iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, o)| ExternalField::of_point_mass(g, o.mass, o.center, at, eps2).acceleration)
                .sum()
        };
        let predicted: Vec<Vec3> = (0..m.len())
            .map(|i| {
                let a = accel(i, m[i].center, &m);
                m[i].center + m[i].velocity * dt + a * (0.5 * dt * dt)
            })
            .collect();
        for (i, system) in self.systems.iter_mut().enumerate() {
            let mut field = ExternalField {
                center: predicted[i],
                ..Default::default()
            };
            for (j, other) in m.iter().enumerate() {
                if j != i {
                    field.add(&ExternalField::of_point_mass(
                        g,
                        other.mass,
                        predicted[j],
                        predicted[i],
                        eps2,
                    ));
                }
            }
            system.engine.set_external_field(field.shifted(system.origin));
        }
    }

    /// Confie des corps (coordonnées globales) au système `target`.
    fn transfer(&mut self, target: usize, mut bodies: BodySet) {
        if bodies.is_empty() {
            return;
        }
        let origin = self.systems[target].origin;
        shift(&mut bodies, -origin);
        if self.systems[target].engine.insert(&bodies) {
            return;
        }
        // Capacité atteinte : reconstruction avec les nouveaux venus.
        shift(&mut bodies, origin);
        let (mut all, step, time) = Self::bodies_of(&self.systems[target]);
        for i in 0..bodies.len() {
            all.push(
                bodies.ids[i],
                bodies.positions[i],
                bodies.velocities[i],
                bodies.masses[i],
            );
        }
        let size = self.systems[target].size;
        if let Ok(s) = self.build(all, origin, size, step, time) {
            self.systems[target] = s;
        }
    }

    /// Détache l'amas `cluster` du système `i` si le critère de séparation
    /// est rempli ; renvoie `true` si c'est fait.
    fn try_split(&mut self, i: usize, cluster: &EjectedCluster) -> bool {
        let parent = &self.systems[i];
        let m = parent.moments;
        let rest_mass = m.mass - cluster.mass;
        if rest_mass <= 0.0 || cluster.members.len() >= parent.engine.body_count() {
            return false;
        }
        let rest_center = (m.center * m.mass - cluster.center * cluster.mass) / rest_mass;
        let distance = (cluster.center - rest_center).length();
        let size = (2.0 * cluster.half_mass_radius).max(self.params.softening);
        if parent.size + size >= SPLIT_RATIO * distance {
            return false;
        }
        let members: HashSet<BodyId> = cluster.members.iter().copied().collect();
        let (all, step, time) = Self::bodies_of(parent);
        let (mut inside, mut outside) = (BodySet::default(), BodySet::default());
        for k in 0..all.len() {
            let target = if members.contains(&all.ids[k]) {
                &mut inside
            } else {
                &mut outside
            };
            target.push(all.ids[k], all.positions[k], all.velocities[k], all.masses[k]);
        }
        if inside.is_empty() || outside.is_empty() {
            return false;
        }
        let (origin, parent_size) = (parent.origin, parent.size);
        let Ok(mut detached) = self.build(inside, cluster.center, size, step, time) else {
            return false;
        };
        let Ok(rest) = self.build(outside, origin, parent_size, step, time) else {
            return false;
        };
        detached.anchor = cluster.anchor;
        self.systems[i] = rest;
        self.systems.push(detached);
        true
    }

    /// Réunit deux sous-systèmes trop proches (une réunion par appel) ;
    /// renvoie `true` si c'est fait.
    fn try_merge(&mut self) -> bool {
        let n = self.systems.len();
        for a in 0..n {
            for b in a + 1..n {
                let (sa, sb) = (&self.systems[a], &self.systems[b]);
                let distance = (sa.moments.center - sb.moments.center).length();
                if sa.size + sb.size <= MERGE_RATIO * distance {
                    continue;
                }
                let (keep, gone) = if sa.moments.mass >= sb.moments.mass {
                    (a, b)
                } else {
                    (b, a)
                };
                let (mut all, step, time) = Self::bodies_of(&self.systems[keep]);
                let (other, _, _) = Self::bodies_of(&self.systems[gone]);
                for k in 0..other.len() {
                    all.push(
                        other.ids[k],
                        other.positions[k],
                        other.velocities[k],
                        other.masses[k],
                    );
                }
                let (origin, anchor) = (self.systems[keep].origin, self.systems[keep].anchor);
                let size = self.systems[keep].size.max(self.systems[gone].size);
                let Ok(mut merged) = self.build(all, origin, size, step, time) else {
                    return false;
                };
                merged.anchor = anchor;
                self.systems[keep] = merged;
                self.systems.remove(gone);
                return true;
            }
        }
        false
    }

    /// Recentre les sous-systèmes dont le barycentre s'est écarté de leur
    /// origine de plus de la moitié de leur taille (précision en simple
    /// précision du moteur GPU) ; renvoie `true` si l'un l'a été.
    fn recenter(&mut self) -> bool {
        let mut changed = false;
        for i in 0..self.systems.len() {
            let s = &self.systems[i];
            if (s.moments.center - s.origin).length() <= 0.5 * s.size {
                continue;
            }
            let (bodies, step, time) = Self::bodies_of(s);
            let (center, size, anchor) = (s.moments.center, s.size, s.anchor);
            if let Ok(mut moved) = self.build(bodies, center, size, step, time) {
                moved.anchor = anchor;
                self.systems[i] = moved;
                changed = true;
            }
        }
        changed
    }
}

impl PhysicsEngine for MultiSystem {
    fn step(&mut self) -> StepReport {
        let multi = self.systems.len() > 1;
        if multi {
            self.apply_fields();
        }
        let root = self.root();
        let mut report = StepReport::default();
        let mut escapees = BodySet::default();
        let mut found: Vec<(usize, EjectedCluster)> = Vec::new();
        let mut cleanup = false;
        for (i, system) in self.systems.iter_mut().enumerate() {
            let r = system.engine.step();
            let m = system.engine.moments();
            system.moments = Moments {
                center: m.center + system.origin,
                ..m
            };
            report.merges.extend(r.merges);
            if multi && i != root {
                // Évadés d'un sous-système secondaire : confiés au principal.
                let mut b = r.removed_bodies;
                shift(&mut b, system.origin);
                for k in 0..b.len() {
                    escapees.push(b.ids[k], b.positions[k], b.velocities[k], b.masses[k]);
                }
            } else {
                report.removed.extend(r.removed);
                let mut b = r.removed_bodies;
                shift(&mut b, system.origin);
                for k in 0..b.len() {
                    report
                        .removed_bodies
                        .push(b.ids[k], b.positions[k], b.velocities[k], b.masses[k]);
                }
            }
            if let Some(clusters) = r.clusters {
                cleanup = true;
                for mut c in clusters {
                    c.center += system.origin;
                    found.push((i, c));
                }
            }
        }
        self.transfer(root, escapees);

        if cleanup {
            let mut changed = false;
            let mut split: HashSet<usize> = HashSet::new();
            let mut listed = Vec::new();
            for (i, c) in found {
                // Une séparation par système et par nettoyage (les indices
                // des corps changent avec la reconstruction).
                if !split.contains(&i) && self.try_split(i, &c) {
                    split.insert(i);
                    changed = true;
                } else {
                    listed.push(c);
                }
            }
            while self.try_merge() {
                changed = true;
            }
            changed |= self.recenter();
            if changed {
                for s in &mut self.systems {
                    let m = s.engine.moments();
                    s.moments = Moments {
                        center: m.center + s.origin,
                        ..m
                    };
                }
                self.publish_view();
            }
            report.clusters = Some(listed);
        }
        report
    }

    fn state(&self) -> SimulationState {
        let mut out = SimulationState::default();
        self.write_state(&mut out);
        out
    }

    fn write_state(&self, out: &mut SimulationState) {
        let mut all = BodySet::default();
        let mut spans = Vec::with_capacity(self.systems.len());
        let (mut step, mut time) = (0, 0.0);
        for system in &self.systems {
            let s = system.engine.state();
            step = s.step;
            time = s.time;
            let start = all.len();
            let b = s.bodies;
            for k in 0..b.len() {
                all.push(b.ids[k], b.positions[k] + system.origin, b.velocities[k], b.masses[k]);
            }
            spans.push(SystemSpan {
                start,
                len: b.len(),
                size: system.size,
            });
        }
        out.bodies = all;
        out.step = step;
        out.time = time;
        out.systems = if spans.len() > 1 { spans } else { Vec::new() };
    }

    fn body_count(&self) -> usize {
        self.systems.iter().map(|s| s.engine.body_count()).sum()
    }

    fn positions_of(&self, ids: &[BodyId]) -> Vec<Option<Vec3>> {
        let mut out = vec![None; ids.len()];
        for system in &self.systems {
            for (o, p) in out.iter_mut().zip(system.engine.positions_of(ids)) {
                if o.is_none() {
                    *o = p.map(|p| p + system.origin);
                }
            }
        }
        out
    }

    fn center_of_mass(&self) -> Option<Vec3> {
        let m = self.moments();
        (m.mass > 0.0).then_some(m.center)
    }

    fn moments(&self) -> Moments {
        let (mut mass, mut mx, mut mv) = (0.0, Vec3::ZERO, Vec3::ZERO);
        for s in &self.systems {
            mass += s.moments.mass;
            mx += s.moments.center * s.moments.mass;
            mv += s.moments.velocity * s.moments.mass;
        }
        if mass > 0.0 {
            Moments {
                mass,
                center: mx / mass,
                velocity: mv / mass,
            }
        } else {
            Moments::default()
        }
    }

    /// Énergies propres des sous-systèmes, plus leurs énergies d'interaction
    /// (sources ponctuelles).
    fn diagnostics(&self) -> Diagnostics {
        let mut d = Diagnostics::default();
        for s in &self.systems {
            let e = s.engine.diagnostics();
            d.kinetic_energy += e.kinetic_energy;
            d.potential_energy += e.potential_energy;
        }
        let eps2 = self.params.softening * self.params.softening;
        for a in 0..self.systems.len() {
            for b in a + 1..self.systems.len() {
                let (ma, mb) = (&self.systems[a].moments, &self.systems[b].moments);
                let r = ((ma.center - mb.center).length_squared() + eps2).sqrt();
                d.potential_energy -= self.params.g * ma.mass * mb.mass / r.max(1e-300);
            }
        }
        d
    }

    fn live_view(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.view
            .as_ref()
            .map(|v| Arc::clone(v) as Arc<dyn Any + Send + Sync>)
    }

    fn systems(&self) -> Vec<SystemSummary> {
        self.systems.iter().map(Subsystem::summary).collect()
    }
}
