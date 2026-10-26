//! Source des états affichés : simulation en direct ou relecture d'un
//! enregistrement. L'écran de visualisation ne dépend que de cette interface.

use std::collections::VecDeque;
use std::sync::Arc;

use stellar_application::{Phase, ReplayPlayer, SimulationHandle, Snapshot, TraceSet};
use stellar_domain::{BodyId, Diagnostics, SimulationConfig, SimulationState};

/// Relecture : lecteur, instantané interpolé courant et traces locales.
pub struct Replay {
    pub player: ReplayPlayer,
    snapshot: Arc<Snapshot>,
    /// Instantanés récents, recyclés pour éviter de réallouer des centaines
    /// de mégaoctets à chaque image.
    recent: VecDeque<Arc<Snapshot>>,
    spare: Option<SimulationState>,
    sequence: u64,
    traces: Option<TraceSet>,
    trace_origins: Vec<BodyId>,
    trace_reference: Option<BodyId>,
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
        // Récupère les tampons d'un instantané que plus personne n'utilise.
        while self.recent.len() > 2 {
            if let Some(old) = self.recent.pop_front() {
                if let Ok(s) = Arc::try_unwrap(old) {
                    self.spare = Some(s.state);
                }
            }
        }
        let mut state = self
            .player
            .swap_state(self.spare.take().unwrap_or_default());
        state.initial_energy = None;
        // Masse initiale : particules de masse 1 au départ de la simulation.
        state.initial_mass = Some(self.player.info().header.config.initial.count as f64);
        // Masse présente : la poussière sous-échantillonnée compte pour son poids.
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

        // Un saut dans la barre de temps redémarre les traces.
        let pos = self.player.clock.position;
        let jumped = (pos - self.last_position).abs() > 1.5;
        self.last_position = pos;
        if let Some(traces) = &mut self.traces {
            if jumped {
                *traces = TraceSet::new(self.trace_origins.iter().copied());
                traces.set_reference(self.trace_reference);
            }
            let player = &self.player;
            traces.resolve_with(|id| player.resolve(id));
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
        }

        self.sequence += 1;
        let snapshot = Arc::new(Snapshot {
            phase: Phase::Ready,
            state,
            diagnostics: Diagnostics::default(),
            top: self.player.top().to_vec(),
            total_mass,
            steps_per_second: 0.0,
            last_step_ms: 0.0,
            running: self.player.clock.playing,
            sequence: self.sequence,
        });
        self.recent.push_back(Arc::clone(&snapshot));
        self.snapshot = snapshot;
    }
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        phase: Phase::Initialising,
        state: SimulationState::default(),
        diagnostics: Diagnostics::default(),
        top: Vec::new(),
        total_mass: 0.0,
        steps_per_second: 0.0,
        last_step_ms: 0.0,
        running: false,
        sequence: 0,
    }
}

pub enum Source {
    Live(SimulationHandle),
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

    /// Fait progresser la relecture (sans effet en direct).
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

    /// Poids d'affichage des particules de poussière (sous-échantillonnage).
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
                set.set_reference(r.trace_reference);
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

    pub fn set_trace_reference(&mut self, id: Option<BodyId>) {
        match self {
            Source::Live(h) => h.set_trace_reference(id),
            Source::Replay(r) => {
                r.trace_reference = id;
                if let Some(t) = &mut r.traces {
                    t.set_reference(id);
                }
            }
        }
    }

    /// Accès en lecture aux traces.
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
