//! Source of the displayed states: live simulation or playback of a
//! recording. The viewing screen depends only on this interface.

use std::collections::VecDeque;
use std::sync::Arc;

use stellar_application::recording::is_dust;
use stellar_application::{
    trace::cluster_members, Phase, ReplayPlayer, SimulationHandle, Snapshot, TraceReference,
    TraceSet,
};
use stellar_domain::{BodyId, Diagnostics, SimulationConfig, SimulationState};
use stellar_domain::{BodySet, Vec3};

/// Playback: player, current interpolated snapshot and local traces.
pub struct Replay {
    pub player: ReplayPlayer,
    snapshot: Arc<Snapshot>,
    /// Recent snapshots, recycled to avoid reallocating hundreds of megabytes
    /// at every frame.
    recent: VecDeque<Arc<Snapshot>>,
    spare: Option<SimulationState>,
    sequence: u64,
    traces: Option<TraceSet>,
    trace_origins: Vec<BodyId>,
    trace_reference: Option<TraceReference>,
    trace_tick: u64,
    last_position: f64,
}

impl Replay {
    pub fn new(player: ReplayPlayer) -> Self {
        Self {
            player,
            snapshot: Arc::new(empty_snapshot()),
            recent: VecDeque::new(),
            spare: None,
            sequence: 0,
            traces: None,
            trace_origins: Vec::new(),
            trace_reference: None,
            trace_tick: 0,
            last_position: 0.0,
        }
    }

    fn tick(&mut self, dt: f64) {
        if !self.player.update(dt) {
            return;
        }
        // Recovers the buffers of a snapshot nobody uses any more.
        while self.recent.len() > 2 {
            if let Some(old) = self.recent.pop_front() {
                if let Ok(s) = Arc::try_unwrap(old) {
                    if let Ok(state) = Arc::try_unwrap(s.state) {
                        self.spare = Some(state);
                    }
                }
            }
        }
        let mut state = self
            .player
            .swap_state(self.spare.take().unwrap_or_default());
        state.initial_energy = None;
        // Initial mass: particles of mass 1 at the start of the simulation.
        state.initial_mass = Some(self.player.info().header.config.initial.count as f64);
        // Present mass: subsampled dust counts for its weight.
        let w = self.player.dust_weight() as f64;
        let total_mass: f64 = state
            .bodies
            .masses
            .iter()
            .map(|&m| {
                if stellar_application::recording::is_dust(m) {
                    m * w
                } else {
                    m
                }
            })
            .sum();

        // A jump in the time bar restarts the traces.
        let pos = self.player.clock.position;
        let jumped = (pos - self.last_position).abs() > 1.5;
        self.last_position = pos;
        if let Some(traces) = &mut self.traces {
            if jumped {
                *traces = TraceSet::new(self.trace_origins.iter().copied());
                traces.set_reference_kind(self.trace_reference);
            }
            let player = &self.player;
            traces.resolve_with(|id| player.resolve(id));
            if let Some(around) = traces.cluster_around() {
                if traces.cluster_size() == 0 || self.trace_tick.is_multiple_of(25) {
                    let params = player.info().header.config.physics;
                    if let Some(m) = cluster_members(&state.bodies, around, &params) {
                        traces.set_cluster_members(m);
                    }
                }
            }
            let ids = traces.current_ids();
            let positions: Vec<_> = ids
                .iter()
                .map(|id| {
                    state
                        .bodies
                        .ids
                        .binary_search(id)
                        .ok()
                        .map(|i| state.bodies.positions[i])
                })
                .collect();
            self.trace_tick += 1;
            traces.push(self.trace_tick, &positions);
            if let Some(c) = weighted_barycenter(&state.bodies, w) {
                traces.push_barycenter(self.trace_tick, c);
            }
        }

        self.sequence += 1;
        let snapshot = Arc::new(Snapshot {
            phase: Phase::Ready,
            step: state.step,
            time: state.time,
            body_count: state.bodies.len(),
            state: Arc::new(state),
            diagnostics: Diagnostics::default(),
            // Derived from the state by the interface (dust weighting).
            center_of_mass: None,
            watched: Vec::new(),
            top: self.player.top().to_vec(),
            total_mass,
            // Ejected clusters are not recorded.
            clusters: Arc::default(),
            clusters_step: None,
            systems: Vec::new(),
            steps_per_second: 0.0,
            last_step_ms: 0.0,
            running: self.player.clock.playing,
            sequence: self.sequence,
            error: None,
        });
        self.recent.push_back(Arc::clone(&snapshot));
        self.snapshot = snapshot;
    }
}

/// Center of mass, with subsampled dust counting for `dust_weight`.
pub fn weighted_barycenter(bodies: &BodySet, dust_weight: f64) -> Option<Vec3> {
    let (m, mx) = bodies.masses.iter().zip(&bodies.positions).fold(
        (0.0, Vec3::ZERO),
        |(m, mx), (&mi, &p)| {
            let w = if is_dust(mi) { mi * dust_weight } else { mi };
            (m + w, mx + p * w)
        },
    );
    (m > 0.0).then(|| mx / m)
}

/// Total angular momentum around the center of mass (same weighting).
pub fn angular_momentum(bodies: &BodySet, dust_weight: f64, center: Vec3) -> Vec3 {
    let (m, mv) = bodies.masses.iter().zip(&bodies.velocities).fold(
        (0.0, Vec3::ZERO),
        |(m, mv), (&mi, &v)| {
            let w = if is_dust(mi) { mi * dust_weight } else { mi };
            (m + w, mv + v * w)
        },
    );
    let v_cm = if m > 0.0 { mv / m } else { Vec3::ZERO };
    bodies
        .masses
        .iter()
        .zip(bodies.positions.iter().zip(&bodies.velocities))
        .map(|(&mi, (&p, &v))| {
            let w = if is_dust(mi) { mi * dust_weight } else { mi };
            (p - center).cross(v - v_cm) * w
        })
        .sum()
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        phase: Phase::Initialising,
        state: Arc::new(SimulationState::default()),
        step: 0,
        time: 0.0,
        body_count: 0,
        diagnostics: Diagnostics::default(),
        center_of_mass: None,
        watched: Vec::new(),
        top: Vec::new(),
        total_mass: 0.0,
        clusters: Arc::default(),
        clusters_step: None,
        systems: Vec::new(),
        steps_per_second: 0.0,
        last_step_ms: 0.0,
        running: false,
        sequence: 0,
        error: None,
    }
}

pub enum Source {
    Live(Box<SimulationHandle>),
    Replay(Box<Replay>),
}

impl Source {
    pub fn config(&self) -> &SimulationConfig {
        match self {
            Source::Live(h) => h.config(),
            Source::Replay(r) => &r.player.info().header.config,
        }
    }

    pub fn live(&self) -> Option<&SimulationHandle> {
        match self {
            Source::Live(h) => Some(h),
            Source::Replay(_) => None,
        }
    }

    pub fn replay(&self) -> Option<&Replay> {
        match self {
            Source::Replay(r) => Some(r),
            Source::Live(_) => None,
        }
    }

    pub fn replay_mut(&mut self) -> Option<&mut Replay> {
        match self {
            Source::Replay(r) => Some(r),
            Source::Live(_) => None,
        }
    }

    /// Advances playback (no effect when live).
    pub fn tick(&mut self, dt: f64) {
        if let Source::Replay(r) = self {
            r.tick(dt);
        }
    }

    pub fn sequence(&self) -> u64 {
        match self {
            Source::Live(h) => h.sequence(),
            Source::Replay(r) => r.sequence,
        }
    }

    pub fn latest(&self) -> Arc<Snapshot> {
        match self {
            Source::Live(h) => h.latest(),
            Source::Replay(r) => Arc::clone(&r.snapshot),
        }
    }

    pub fn resolve(&self, id: BodyId) -> Option<BodyId> {
        match self {
            Source::Live(h) => h.resolve(id),
            Source::Replay(r) => r.player.resolve(id),
        }
    }

    /// Display weight of the dust particles (subsampling).
    pub fn dust_weight(&self) -> f32 {
        match self {
            Source::Live(_) => 1.0,
            Source::Replay(r) => r.player.dust_weight(),
        }
    }

    pub fn start_traces(&mut self, ids: Vec<BodyId>) {
        match self {
            Source::Live(h) => h.start_traces(ids),
            Source::Replay(r) => {
                r.trace_origins = ids.clone();
                let mut set = TraceSet::new(ids);
                set.set_reference_kind(r.trace_reference);
                let state = &r.snapshot.state;
                let positions: Vec<_> = set
                    .current_ids()
                    .iter()
                    .map(|id| {
                        state
                            .bodies
                            .ids
                            .binary_search(id)
                            .ok()
                            .map(|i| state.bodies.positions[i])
                    })
                    .collect();
                r.trace_tick += 1;
                set.push(r.trace_tick, &positions);
                if let Some(c) = weighted_barycenter(&state.bodies, r.player.dust_weight() as f64) {
                    set.push_barycenter(r.trace_tick, c);
                }
                r.traces = Some(set);
            }
        }
    }

    pub fn stop_traces(&mut self) {
        match self {
            Source::Live(h) => h.stop_traces(),
            Source::Replay(r) => r.traces = None,
        }
    }

    pub fn set_trace_reference(&mut self, id: Option<TraceReference>) {
        match self {
            Source::Live(h) => h.set_trace_reference(id),
            Source::Replay(r) => {
                r.trace_reference = id;
                if let Some(t) = &mut r.traces {
                    t.set_reference_kind(id);
                }
            }
        }
    }

    /// Read access to the traces.
    pub fn with_traces<R>(&self, f: impl FnOnce(Option<&TraceSet>) -> R) -> R {
        match self {
            Source::Live(h) => f(h.traces().as_ref()),
            Source::Replay(r) => f(r.traces.as_ref()),
        }
    }

    pub fn shutdown(self) {
        if let Source::Live(h) = self {
            h.shutdown();
        }
    }
}
