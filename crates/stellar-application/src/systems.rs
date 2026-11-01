//! Composite engine: a simulation split into subsystems.
//!
//! Each subsystem is computed by its own engine (created by the same
//! factory), in its own frame: positions relative to an origin near its
//! barycenter, which preserves the precision of a cluster gone very far
//! away. Subsystems advance with the same step; each one feels the others
//! through a linear field (see [`stellar_domain::systems`]), computed from
//! the barycenters extrapolated to the end of the step.
//!
//! At each cleanup:
//! - an ejected cluster far enough from its system becomes a subsystem;
//! - two subsystems that are too close are merged;
//! - a subsystem whose barycenter has moved away from its origin is
//!   recentred;
//! - bodies removed by a secondary subsystem (escapees) are not lost: they
//!   join the main system (the most massive one), whose cleanup decides
//!   their fate;
//! - with automatic pruning, secondary subsystems reduced to a single
//!   negligible escaping body are removed
//!   ([`MultiSystem::prune_negligible`], also available on request).
//!
//! The rest of the application sees an ordinary engine; with a single
//! subsystem (the usual case), the composite only forwards calls. Details:
//! `docs/subsystems.md`.

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use stellar_domain::systems::{negligible_escapee, ViewPart, MERGE_RATIO, SPLIT_RATIO};
use stellar_domain::{
    ranking, BodyId, BodySet, Diagnostics, EjectedCluster, EngineFactory, ExternalField, Moments,
    PhysicsEngine, PhysicsParams, SimulationState, StepReport, SystemSpan, SystemSummary,
    SystemsView, Vec3,
};

struct Subsystem {
    engine: Box<dyn PhysicsEngine>,
    /// Origin of the engine's frame (global coordinates).
    origin: Vec3,
    size: f64,
    anchor: BodyId,
    /// Moments at the last step (global coordinates).
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

/// Composite engine (see the module).
pub struct MultiSystem {
    factory: Arc<dyn EngineFactory>,
    params: PhysicsParams,
    initial_mass: Option<f64>,
    systems: Vec<Subsystem>,
    /// Composite live view, if the engines offer one.
    view: Option<Arc<SystemsView>>,
    /// Negligible lone bodies removed at every cleanup.
    auto_prune: bool,
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
    /// Builds the subsystems described by `state.systems` (a single one if the
    /// list is empty; its frame is then the state's).
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
            auto_prune: false,
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

    /// Creates the engine of a subsystem from bodies in global coordinates.
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

    /// Bodies of a subsystem, in global coordinates.
    fn bodies_of(system: &Subsystem) -> (BodySet, u64, f64) {
        let s = system.engine.state();
        let mut bodies = s.bodies;
        shift(&mut bodies, system.origin);
        (bodies, s.step, s.time)
    }

    /// Index of the main system (the most massive).
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
                            anchor: s.anchor,
                        })
                    })
                    .collect(),
            );
        }
    }

    /// External fields valid at the end of the step: extrapolated barycenters
    /// (velocity and mutual attraction), point sources.
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

    /// Hands bodies (global coordinates) over to system `target`.
    fn transfer(&mut self, target: usize, mut bodies: BodySet) {
        if bodies.is_empty() {
            return;
        }
        let origin = self.systems[target].origin;
        shift(&mut bodies, -origin);
        if self.systems[target].engine.insert(&bodies) {
            return;
        }
        // Capacity reached: rebuild with the newcomers.
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

    /// Detaches the cluster `cluster` from system `i` if the split criterion is
    /// met; returns `true` if done.
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

    /// Merges two subsystems that are too close (one merge per call); returns
    /// `true` if done.
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

    /// Removes at each cleanup the negligible lone bodies (see
    /// [`Self::prune_negligible`]).
    pub fn set_auto_prune(&mut self, on: bool) {
        self.auto_prune = on;
    }

    /// Removes the secondary subsystems reduced to a single body escaping the
    /// rest of the simulation with a negligible influence on it
    /// ([`stellar_domain::systems::negligible_escapee`]; rest: every other
    /// subsystem, of the main system's size). Returns the removals (global
    /// coordinates), to be handled like those of a step.
    pub fn prune_negligible(&mut self) -> StepReport {
        let mut report = StepReport::default();
        if self.systems.len() < 2 {
            return report;
        }
        let root = self.root();
        let rest_size = self.systems[root].size;
        let total = self.moments();
        let g = self.params.g;
        let gone: Vec<usize> = (0..self.systems.len())
            .filter(|&i| i != root && self.systems[i].engine.body_count() == 1)
            .filter(|&i| {
                let m = self.systems[i].moments;
                let rest_mass = total.mass - m.mass;
                if rest_mass <= 0.0 {
                    return false;
                }
                let rest = Moments {
                    mass: rest_mass,
                    center: (total.center * total.mass - m.center * m.mass) / rest_mass,
                    velocity: (total.velocity * total.mass - m.velocity * m.mass) / rest_mass,
                };
                negligible_escapee(g, m.mass, m.center, m.velocity, &rest, rest_size)
            })
            .collect();
        for &i in gone.iter().rev() {
            let system = self.systems.remove(i);
            let (bodies, _, _) = Self::bodies_of(&system);
            for k in 0..bodies.len() {
                report.removed.push(bodies.ids[k]);
                report
                    .removed_bodies
                    .push(bodies.ids[k], bodies.positions[k], bodies.velocities[k], bodies.masses[k]);
            }
        }
        if !gone.is_empty() {
            self.publish_view();
        }
        report
    }

    /// Current positions of the requested bodies, with the marker body of the
    /// subsystem holding each one.
    pub fn locate(&self, ids: &[BodyId]) -> Vec<Option<(Vec3, BodyId)>> {
        let mut out = vec![None; ids.len()];
        for system in &self.systems {
            for (o, p) in out.iter_mut().zip(system.engine.positions_of(ids)) {
                if o.is_none() {
                    *o = p.map(|p| (p + system.origin, system.anchor));
                }
            }
        }
        out
    }

    /// Recentres the subsystems whose barycenter has moved away from their
    /// origin by more than half their size (to preserve the GPU engine's
    /// single precision); returns `true` if one was.
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
                // Escapees of a secondary subsystem: handed over to the main one.
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
                // One split per system and per cleanup (body indices change with the
                // rebuild).
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
            if self.auto_prune {
                let pruned = self.prune_negligible();
                report.removed.extend(pruned.removed);
                let b = pruned.removed_bodies;
                for k in 0..b.len() {
                    report
                        .removed_bodies
                        .push(b.ids[k], b.positions[k], b.velocities[k], b.masses[k]);
                }
            }
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
        self.locate(ids).into_iter().map(|o| o.map(|(p, _)| p)).collect()
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

    /// Own energies of the subsystems, plus their interaction energies (point
    /// sources).
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
