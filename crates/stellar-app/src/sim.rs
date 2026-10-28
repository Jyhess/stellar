//! Écran de simulation : navigation, sélection, HUD, sauvegarde.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Pos2, RichText, Stroke};
use eframe::egui_wgpu;
use glam::DVec3;
use rayon::prelude::*;
use stellar_application::recording::is_dust;
use stellar_application::trace::{relative_to, MAX_TAIL};
use stellar_application::{Phase, SimulatorService, Snapshot, TraceReference, ViewState};
use stellar_domain::physics::body_radius;
use stellar_domain::{ranking, BodyId, SystemsView};

use crate::camera::{Camera, Mode, MAX_FOV, MIN_FOV};
use crate::format;
use crate::picking::{pick, visible_top_k};
use crate::render::{mass_color, Instance, InstanceBatch, PointsCallback, PointsSource, Uniforms};
use crate::source::{angular_momentum, weighted_barycenter, Source};

use crate::i18n::tr;

/// Luminosité initiale : le mélange additif sature quand les corps se
/// superposent, d'où une atténuation avec leur nombre (1 pour un million).
fn default_brightness(count: usize) -> f32 {
    (1e6 / count.max(1) as f64).powf(0.75).clamp(0.05, 1.5) as f32
}

/// Axe unitaire du moment cinétique total autour de `center` ; `None` en
/// l'absence de rotation d'ensemble significative.
fn rotation_axis(
    bodies: &stellar_domain::BodySet,
    dust_weight: f64,
    center: DVec3,
) -> Option<DVec3> {
    let l = angular_momentum(bodies, dust_weight, center);
    // Échelle : moment cinétique qu'aurait le système si tout tournait dans le même sens.
    let scale: f64 = bodies
        .masses
        .iter()
        .zip(bodies.positions.iter().zip(&bodies.velocities))
        .map(|(&m, (&p, &v))| m * (p - center).length() * v.length())
        .sum();
    (l.length() > 1e-3 * scale && scale > 0.0).then(|| l.normalize())
}

/// Part exprimée en pourcentage, avec une précision adaptée.
fn format_share(f: f64) -> String {
    let p = 100.0 * f;
    if p >= 10.0 {
        format!("{p:.1} %")
    } else if p >= 0.01 || p == 0.0 {
        format!("{p:.2} %")
    } else {
        format!("{p:.1e} %")
    }
    .replace('.', crate::format::decimal_sep())
}

pub fn format_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        tr!("{} Go", format!("{:.2}", b / 1e9))
    } else {
        tr!("{} Mo", format!("{:.0}", b / 1e6))
    }
    .replace('.', crate::format::decimal_sep())
}

pub enum SimAction {
    None,
    BackHome,
    Quit,
}

const MOUSE_SENSITIVITY: f64 = 0.0022;
const SCROLL_SENSITIVITY: f64 = 0.0025;
/// Vitesse de bascule autour de l'axe de visée (rad/s).
const ROLL_SPEED: f64 = 1.6;
/// Multiplicateur de vitesse initial : fraction de la distance à la
/// référence parcourue chaque seconde.
const DEFAULT_SPEED: f64 = 0.5;
/// Rythme du redressement de l'horizon (1/s).
const LEVEL_RATE: f64 = 6.0;
/// Intervalle de recalcul de la normale au disque (grille affichée).
const DISK_NORMAL_REFRESH: Duration = Duration::from_secs(5);
/// Demi-étendue de la grille du plan du disque, en mailles.
const GRID_HALF_LINES: i64 = 24;
/// Découpage de chaque ligne de la grille (pour l'estompage).
const GRID_SUBDIVISIONS: usize = 24;
const PICK_TOLERANCE_PX: f64 = 10.0;
const TOAST_DURATION: Duration = Duration::from_secs(4);
/// Nombre de corps tracés.
const TRACE_COUNT: usize = 10;
/// Couleurs des traces, une par corps tracé.
const TRACE_PALETTE: [Color32; 10] = [
    Color32::from_rgb(255, 99, 71),
    Color32::from_rgb(255, 200, 40),
    Color32::from_rgb(120, 230, 90),
    Color32::from_rgb(60, 200, 255),
    Color32::from_rgb(190, 120, 255),
    Color32::from_rgb(255, 120, 200),
    Color32::from_rgb(255, 150, 60),
    Color32::from_rgb(80, 255, 200),
    Color32::from_rgb(150, 170, 255),
    Color32::from_rgb(230, 230, 230),
];
/// Fraction de la masse cadrée par la vue d'ensemble.
const OVERVIEW_MASS_FRACTION: f64 = 0.9;
/// Marge autour de la sphère cadrée.
const OVERVIEW_MARGIN: f64 = 1.05;

/// Données dérivées d'un instantané, recalculées à chaque publication.
struct SnapshotCache {
    barycenter: DVec3,
    /// Vitesse du centre de masse.
    barycenter_velocity: DVec3,
    /// Axe du moment cinétique total (calculé quand le barycentre est sélectionné).
    rotation_axis: Option<DVec3>,
    selected_index: Option<usize>,
    selected_rank: Option<usize>,
    /// Masses des corps tracés (identifiant courant → masse).
    trace_masses: std::collections::HashMap<BodyId, f64>,
}

pub struct SimScreen {
    source: Source,
    service: SimulatorService,
    camera: Camera,
    captured: bool,
    capture_applied: Option<bool>,
    menu_open: bool,
    selected: Option<BodyId>,
    /// Le barycentre est sélectionné (à la place d'un corps).
    bary_selected: bool,
    paused: bool,

    snapshot: Arc<Snapshot>,
    seen_sequence: u64,
    cache: SnapshotCache,
    /// Corps du moteur GPU, dessinés directement (sans copie) quand le
    /// moteur calcule sur la carte de l'interface.
    direct: Option<Arc<SystemsView>>,
    /// Corps dont le thread de calcul publie la position à chaque instantané.
    sent_watch: Option<BodyId>,

    anchor: DVec3,
    batch: Option<InstanceBatch>,
    generation: u64,
    need_upload: bool,

    brightness: f32,
    dust_floor: f32,
    min_pixels: f32,
    limit_rate: bool,
    max_rate: f64,

    fps: f64,
    last_frame: Option<Instant>,
    /// Journal de mesure (`STELLAR_STATS=1`) : dernière ligne écrite.
    last_stats: Option<Instant>,
    last_aspect: f64,
    toast: Option<(String, Instant)>,
    save_job: Option<Receiver<Result<PathBuf, String>>>,

    traces_active: bool,
    /// Référentiel d'affichage des traces.
    trace_frame: TraceFrame,
    /// Référence transmise au thread de calcul.
    sent_reference: Option<TraceReference>,
    /// Affiche seulement la queue récente des traces.
    trace_tail_only: bool,
    trace_tail_len: usize,

    /// Enregistrement : un instantané tous les N pas.
    record_interval: u32,
    record_budget_enabled: bool,
    record_budget: u64,

    /// Grille du plan du disque affichée.
    show_grid: bool,
    /// Redressement progressif de l'horizon en cours (touche H).
    leveling: bool,
    /// Normale au plan du disque (axe du moment cinétique total), recalculée
    /// de temps en temps.
    disk_normal: Option<DVec3>,
    disk_normal_at: Option<Instant>,
}

impl SimScreen {
    pub fn new(source: Source, service: SimulatorService, view: Option<ViewState>) -> Self {
        let r = source.config().physics.reference_radius.max(1e-9);
        let speed = DEFAULT_SPEED;
        let camera = match &view {
            Some(v) => Camera::from_view(v, speed),
            None => Camera::looking_at(DVec3::new(0.0, -2.6 * r, 0.9 * r), DVec3::ZERO, speed),
        };
        let bary_selected = matches!(camera.mode, Mode::Barycenter { .. });
        let selected = camera.followed();
        let snapshot = source.latest();
        let paused = !snapshot.running;
        let handle_count = match source.replay() {
            Some(r) => r
                .player
                .info()
                .frames
                .first()
                .map_or(1, |f| f.count as usize),
            None => source
                .config()
                .initial
                .count
                .max(snapshot.state.bodies.len()),
        };
        Self {
            source,
            service,
            camera,
            captured: true,
            capture_applied: None,
            menu_open: false,
            selected,
            bary_selected,
            paused,
            seen_sequence: u64::MAX,
            snapshot,
            direct: None,
            sent_watch: None,
            cache: SnapshotCache {
                barycenter: DVec3::ZERO,
                barycenter_velocity: DVec3::ZERO,
                rotation_axis: None,
                selected_index: None,
                selected_rank: None,
                trace_masses: Default::default(),
            },
            anchor: DVec3::ZERO,
            batch: None,
            generation: 0,
            need_upload: true,
            brightness: default_brightness(handle_count),
            dust_floor: 0.2,
            min_pixels: 1.6,
            limit_rate: false,
            max_rate: 30.0,
            fps: 0.0,
            last_frame: None,
            last_stats: std::env::var_os("STELLAR_STATS").map(|_| Instant::now()),
            last_aspect: 16.0 / 9.0,
            toast: None,
            save_job: None,
            traces_active: false,
            trace_frame: TraceFrame::Barycenter,
            sent_reference: None,
            trace_tail_only: false,
            trace_tail_len: 600,
            record_interval: 10,
            record_budget_enabled: true,
            record_budget: 1_000_000,
            show_grid: false,
            leveling: false,
            disk_normal: None,
            disk_normal_at: None,
        }
    }

    fn reference_radius(&self) -> f64 {
        self.source.config().physics.reference_radius.max(1e-9)
    }

    fn base_radius(&self) -> f64 {
        self.source.config().physics.base_radius
    }

    /// Vrai si un enregistrement est en cours.
    pub fn is_recording(&self) -> bool {
        self.source.live().is_some_and(|h| h.is_recording())
    }

    /// Suspend l'enregistrement en cours (avec sauvegarde de reprise liée).
    pub fn suspend_recording(&self) -> Option<Result<PathBuf, String>> {
        let h = self.source.live()?;
        if !h.is_recording() {
            return None;
        }
        match self
            .service
            .suspend_recording(h, Some(self.camera.to_view()))
        {
            Ok(Some(p)) => Some(Ok(p)),
            Ok(None) => None,
            Err(e) => Some(Err(e.to_string())),
        }
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    // ------------------------------------------------------------------
    // Instantanés
    // ------------------------------------------------------------------

    fn poll_snapshot(&mut self) {
        let seq = self.source.sequence();
        if seq == self.seen_sequence {
            return;
        }
        let first = self.seen_sequence == u64::MAX;
        self.seen_sequence = seq;
        let previous = std::mem::replace(&mut self.snapshot, self.source.latest());
        if self.direct.is_none() {
            self.direct = self
                .source
                .live()
                .and_then(|h| h.live_view())
                .and_then(|v| v.downcast::<SystemsView>().ok());
        }
        // Les grandeurs tirées de l'état complet ne sont recalculées que
        // lorsqu'il change (avec l'affichage direct, la plupart des
        // publications ne portent que les grandeurs globales).
        let state_changed = first || !Arc::ptr_eq(&previous.state, &self.snapshot.state);
        if state_changed {
            let bodies = &self.snapshot.state.bodies;
            let w = self.source.dust_weight() as f64;
            self.cache.barycenter = weighted_barycenter(bodies, w).unwrap_or(DVec3::ZERO);
            let total = self.snapshot.total_mass.max(1e-300);
            self.cache.barycenter_velocity = bodies
                .masses
                .iter()
                .zip(&bodies.velocities)
                .map(|(&m, &v)| v * if is_dust(m) { m * w } else { m })
                .sum::<DVec3>()
                / total;
            self.cache.rotation_axis = if self.bary_selected {
                rotation_axis(bodies, w, self.cache.barycenter)
            } else {
                None
            };
            if self.bary_selected {
                self.disk_normal = self.cache.rotation_axis;
                self.disk_normal_at = Some(Instant::now());
            }
            self.refresh_trace_masses();
        }
        // Centre de masse courant, quand le moteur le publie.
        if let Some(c) = self.snapshot.center_of_mass {
            self.cache.barycenter = c;
        }
        let selected_before = self.selected;

        // Suivi de la sélection à travers fusions et éjections.
        if let Some(id) = self.selected {
            if self.snapshot.phase == Phase::Ready {
                match self.source.resolve(id) {
                    Some(new_id) => {
                        if new_id != id {
                            self.selected = Some(new_id);
                            if let Mode::Orbit { target, .. } = &mut self.camera.mode {
                                *target = new_id;
                            }
                        }
                    }
                    None => {
                        self.selected = None;
                        self.camera.mode = Mode::Free;
                        self.notify(tr("Le corps suivi a été éjecté et éliminé."));
                    }
                }
            }
        }
        if state_changed || self.selected != selected_before {
            self.refresh_selection_cache();
            self.need_upload = true;
        }
    }

    /// Demande au thread de calcul la position courante du corps
    /// sélectionné à chaque publication (l'état complet peut dater).
    fn sync_watch(&mut self) {
        if self.selected == self.sent_watch {
            return;
        }
        if let Some(h) = self.source.live() {
            h.set_watched(self.selected.into_iter().collect());
        }
        self.sent_watch = self.selected;
    }

    /// Position courante du corps sélectionné : publiée à chaque instantané
    /// s'il est suivi, sinon tirée de l'état complet.
    fn selected_position(&self) -> Option<DVec3> {
        self.snapshot.watched_position(self.selected?).or_else(|| {
            self.cache
                .selected_index
                .map(|i| self.snapshot.state.bodies.positions[i])
        })
    }

    fn refresh_selection_cache(&mut self) {
        let bodies = &self.snapshot.state.bodies;
        self.cache.selected_index = self.selected.and_then(|id| bodies.index_of(id));
        self.cache.selected_rank = self.cache.selected_index.map(|i| {
            ranking::rank_of(
                &bodies.ids,
                &bodies.masses,
                (bodies.masses[i], bodies.ids[i]),
            )
        });
        // Absent de l'état complet (qui peut dater) et non suivi : perdu.
        if self.cache.selected_index.is_none()
            && self.snapshot.phase == Phase::Ready
            && self
                .selected
                .is_some_and(|id| self.snapshot.position_of(id).is_none())
        {
            self.selected = None;
            self.camera.mode = Mode::Free;
        }
    }

    /// Suit le corps d'indice `i`. Si un corps est déjà suivi, la direction
    /// de visée, le zoom et la distance à la cible sont conservés ; depuis le
    /// vol libre, la caméra se tourne vers le corps sans changer de distance.
    fn select_index(&mut self, i: usize) {
        let bodies = &self.snapshot.state.bodies;
        let p = bodies.positions[i];
        let id = bodies.ids[i];
        let r = body_radius(bodies.masses[i], self.base_radius());
        let distance = match self.camera.mode {
            Mode::Orbit { distance, .. } | Mode::Barycenter { distance } => distance,
            Mode::Free => {
                self.camera.look_towards(p - self.camera.position);
                (p - self.camera.position).length()
            }
        };
        // Seule exception : ne jamais se retrouver à l'intérieur du corps.
        let distance = distance.max(1.5 * r).max(1e-9);
        self.selected = Some(id);
        self.bary_selected = false;
        self.camera.mode = Mode::Orbit {
            target: id,
            distance,
        };
        self.refresh_selection_cache();
        self.camera.place_in_orbit(p);
    }

    /// Active le tracé des 10 corps les plus massifs visibles à l'écran, ou l'arrête.
    fn toggle_traces(&mut self) {
        if self.traces_active {
            self.traces_active = false;
            self.source.stop_traces();
            self.notify(tr("Traces effacées."));
            return;
        }
        if self.snapshot.phase != Phase::Ready {
            self.notify(tr("La simulation n'est pas encore prête."));
            return;
        }
        // Les 10 corps les plus massifs parmi ceux visibles à l'écran.
        let ids = visible_top_k(
            &self.snapshot.state.bodies,
            &self.camera,
            self.last_aspect,
            TRACE_COUNT,
        );
        if ids.is_empty() {
            self.notify(tr("Aucun corps visible à l'écran : rien à tracer."));
            return;
        }
        let n = ids.len();
        self.source.start_traces(ids);
        self.traces_active = true;
        self.sent_reference = None; // la référence sera retransmise
        self.sync_trace_reference();
        self.notify(tr!("Traces activées sur les {} corps visibles les plus massifs.", n));
    }

    /// Transmet au thread de calcul le corps de référence des traces
    /// (le corps sélectionné, en mode relatif).
    fn sync_trace_reference(&mut self) {
        let wanted = if self.traces_active {
            self.trace_reference()
        } else {
            None
        };
        if wanted == self.sent_reference {
            return;
        }
        // Si la sélection a seulement suivi une fusion, la piste suit d'elle-même.
        let continuation = match (self.sent_reference, wanted) {
            (Some(TraceReference::Body(sent)), Some(TraceReference::Body(w)))
            | (Some(TraceReference::Cluster(sent)), Some(TraceReference::Cluster(w))) => {
                self.source.resolve(sent) == Some(w)
            }
            _ => false,
        };
        if !continuation && self.traces_active {
            self.source.set_trace_reference(wanted);
        }
        self.sent_reference = wanted;
    }

    /// Référentiel effectif des traces : sans corps sélectionné, le
    /// barycentre de la simulation.
    fn trace_reference(&self) -> Option<TraceReference> {
        let body = self.selected.filter(|_| !self.bary_selected);
        Some(match (self.trace_frame, body) {
            (TraceFrame::Body, Some(id)) => TraceReference::Body(id),
            (TraceFrame::Cluster, Some(id)) => TraceReference::Cluster(id),
            _ => TraceReference::Barycenter,
        })
    }

    /// Référentiel suivant (touche R).
    fn cycle_trace_frame(&mut self) {
        self.trace_frame = match self.trace_frame {
            TraceFrame::Body => TraceFrame::Barycenter,
            TraceFrame::Barycenter => TraceFrame::Cluster,
            TraceFrame::Cluster => TraceFrame::Body,
        };
        let mut text = tr!("Référentiel des traces : {}", self.trace_frame.label());
        if self.trace_frame != TraceFrame::Barycenter && self.trace_reference() == Some(TraceReference::Barycenter) {
            text.push_str(tr(" — sélectionnez un corps (barycentre en attendant)"));
        }
        self.notify(text);
    }

    /// Sélectionne le barycentre et le regarde perpendiculairement au plan
    /// du disque (selon l'axe du moment cinétique total), en conservant la
    /// distance à la cible. Sans rotation d'ensemble, la vue suit l'axe Oz.
    fn view_barycenter_from_axis(&mut self) {
        let bodies = &self.snapshot.state.bodies;
        if bodies.is_empty() {
            return;
        }
        let c = self.cache.barycenter;
        let w = self.source.dust_weight() as f64;
        let axis = rotation_axis(bodies, w, c);
        let distance = self
            .camera
            .orbit_distance()
            .unwrap_or_else(|| (c - self.camera.position).length())
            .max(1e-9);
        self.camera.look_towards(-axis.unwrap_or(DVec3::Z));
        self.camera.mode = Mode::Barycenter { distance };
        self.camera.place_in_orbit(c);
        self.selected = None;
        self.bary_selected = true;
        self.cache.selected_index = None;
        self.cache.selected_rank = None;
        self.cache.rotation_axis = axis;
        self.disk_normal = axis;
        self.disk_normal_at = Some(Instant::now());
        self.notify(match axis {
            Some(a) => tr!("Barycentre vu de face au disque (axe incliné de {}° sur Oz).", format!("{:.1}", a.angle_between(DVec3::Z).to_degrees()))
            .replace('.', crate::format::decimal_sep()),
            None => tr("Barycentre : aucune rotation d'ensemble, vue selon l'axe Oz.").to_owned(),
        });
    }

    /// Oriente la visée vers le centre de gravité (vol libre).
    fn aim_at_barycenter(&mut self) {
        if self.snapshot.state.bodies.is_empty() {
            return;
        }
        self.camera.mode = Mode::Free;
        self.camera
            .look_towards(self.cache.barycenter - self.camera.position);
    }

    /// Recalcule la normale au plan du disque : toujours si `force`, sinon
    /// une première fois puis périodiquement tant que la grille est affichée.
    fn refresh_disk_normal(&mut self, force: bool) {
        if self.snapshot.phase != Phase::Ready || self.snapshot.state.bodies.is_empty() {
            return;
        }
        let due = match self.disk_normal_at {
            None => true,
            Some(t) => self.show_grid && t.elapsed() >= DISK_NORMAL_REFRESH,
        };
        if !(force || due) {
            return;
        }
        let w = self.source.dust_weight() as f64;
        self.disk_normal = rotation_axis(&self.snapshot.state.bodies, w, self.cache.barycenter);
        self.disk_normal_at = Some(Instant::now());
    }

    /// Haut de référence : normale au disque, sinon Oz.
    fn reference_up(&self) -> DVec3 {
        self.disk_normal.unwrap_or(DVec3::Z)
    }

    /// Lance le redressement progressif de l'horizon (touche H).
    fn start_leveling(&mut self) {
        self.refresh_disk_normal(true);
        if self.camera.roll_to(self.reference_up()).is_some() {
            self.leveling = true;
        } else {
            self.notify(tr("Visée selon la normale au disque : rien à redresser."));
        }
    }

    /// Point de référence de la vitesse adaptative : la sélection (corps ou
    /// barycentre), sinon le barycentre.
    fn speed_reference(&self) -> Option<(DVec3, f64)> {
        if !self.bary_selected {
            if let (Some(i), Some(p)) = (self.cache.selected_index, self.selected_position()) {
                let r = body_radius(self.snapshot.state.bodies.masses[i], self.base_radius());
                return Some((p, r));
            }
        }
        (!self.snapshot.state.bodies.is_empty()).then_some((self.cache.barycenter, 0.0))
    }

    /// Vitesse de déplacement en vol libre (unités/s), avant Maj / Ctrl :
    /// proportionnelle à la distance à la référence.
    fn free_speed(&self) -> f64 {
        let floor = 1e-4 * self.reference_radius();
        let d = match self.speed_reference() {
            Some((p, r)) => (p - self.camera.position).length().max(2.0 * r),
            None => self.reference_radius(),
        };
        self.camera.speed * d.max(floor)
    }

    /// Touche F : quitte le suivi (la vue reste telle quelle) ou suit la
    /// sélection (corps ou barycentre).
    fn toggle_follow(&mut self) {
        if self.camera.is_orbiting() {
            self.camera.mode = Mode::Free;
            self.notify(tr("Suivi interrompu : vol libre."));
            return;
        }
        if self.bary_selected {
            let c = self.cache.barycenter;
            let distance = (c - self.camera.position).length().max(1e-9);
            self.camera.look_towards(c - self.camera.position);
            self.camera.mode = Mode::Barycenter { distance };
            self.camera.place_in_orbit(c);
            self.notify(tr("Suivi du barycentre."));
        } else if let Some(i) = self.cache.selected_index {
            self.select_index(i);
            self.notify(tr("Suivi du corps sélectionné."));
        } else {
            self.notify(
                tr("Rien à suivre : sélectionnez un corps (clic, Origine) ou le barycentre (B)."),
            );
        }
    }

    /// Recule la caméra, dans sa direction de visée actuelle, jusqu'à voir la
    /// sphère centrée sur le barycentre qui contient 90 % de la masse.
    fn frame_mass(&mut self) {
        let bodies = &self.snapshot.state.bodies;
        if bodies.is_empty() {
            return;
        }
        let c = self.cache.barycenter;
        let r90 = bodies.mass_radius(c, OVERVIEW_MASS_FRACTION).max(1e-9);
        let half_v = 0.5 * self.camera.fov_y;
        let half_h = (half_v.tan() * self.last_aspect.max(1e-3)).atan();
        let half = half_v.min(half_h);
        let d = OVERVIEW_MARGIN * r90 / half.sin();
        self.camera.mode = Mode::Free;
        self.camera.position = c - self.camera.forward() * d;
        self.notify(tr!("Vue d'ensemble : 90 % de la masse dans un rayon de {}", format::real(r90)));
    }

    fn select_heaviest(&mut self) {
        let b = &self.snapshot.state.bodies;
        if let Some(i) = ranking::heaviest(&b.ids, &b.masses) {
            self.select_index(i);
        }
    }

    fn select_neighbour(&mut self, lighter: bool) {
        let Some(cur) = self.cache.selected_index else {
            self.select_heaviest();
            return;
        };
        let b = &self.snapshot.state.bodies;
        if let Some(i) = ranking::neighbour(&b.ids, &b.masses, (b.masses[cur], b.ids[cur]), lighter)
        {
            self.select_index(i);
        } else {
            self.notify(if lighter {
                tr("C'est déjà le corps le plus léger.")
            } else {
                tr("C'est déjà le corps le plus massif.")
            });
        }
    }

    // ------------------------------------------------------------------
    // Entrées
    // ------------------------------------------------------------------

    fn set_captured(&mut self, captured: bool) {
        self.captured = captured;
    }

    fn apply_capture(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if !focused && self.captured {
            self.captured = false;
        }
        let want = self.captured && !self.menu_open;
        if self.capture_applied != Some(want) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(if want {
                egui::CursorGrab::Confined
            } else {
                egui::CursorGrab::None
            }));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!want));
            self.capture_applied = Some(want);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) -> SimAction {
        let mut action = SimAction::None;
        let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));

        if pressed(Key::Escape) {
            self.menu_open = !self.menu_open;
        }
        if pressed(Key::Tab) {
            let c = !self.captured;
            self.set_captured(c);
        }
        if pressed(Key::F5) {
            self.start_save();
        }
        if self.menu_open {
            return action;
        }
        if pressed(Key::Space) {
            self.toggle_pause();
        }
        if pressed(Key::P) {
            self.step_once();
        }
        if pressed(Key::F9) {
            self.toggle_recording();
        }
        if let Some(r) = self.source.replay_mut() {
            let clock = &mut r.player.clock;
            if pressed(Key::ArrowRight) {
                clock.step(1);
            }
            if pressed(Key::ArrowLeft) {
                clock.step(-1);
            }
            if pressed(Key::Backspace) {
                clock.reverse = !clock.reverse;
            }
            if pressed(Key::ArrowUp) {
                clock.speed = (clock.speed * 1.5).min(1000.0);
            }
            if pressed(Key::ArrowDown) {
                clock.speed = (clock.speed / 1.5).max(0.05);
            }
        }
        if pressed(Key::T) {
            self.toggle_traces();
        }
        if pressed(Key::R) {
            self.cycle_trace_frame();
        }
        if pressed(Key::B) {
            self.view_barycenter_from_axis();
        }
        if pressed(Key::C) {
            self.aim_at_barycenter();
        }
        if pressed(Key::V) {
            self.frame_mass();
        }
        if pressed(Key::F) {
            self.toggle_follow();
        }
        if pressed(Key::H) {
            self.start_leveling();
        }
        if pressed(Key::G) {
            self.show_grid = !self.show_grid;
            if self.show_grid {
                self.refresh_disk_normal(true);
            }
        }
        if pressed(Key::Home) {
            self.select_heaviest();
        }
        if pressed(Key::PageDown) {
            self.select_neighbour(true);
        }
        if pressed(Key::PageUp) {
            self.select_neighbour(false);
        }
        if ctx.input(|i| i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals)) {
            self.camera.speed *= 1.5;
        }
        if ctx.input(|i| i.key_pressed(Key::Minus)) {
            self.camera.speed /= 1.5;
        }
        if ctx.input(|i| i.key_pressed(Key::F10)) {
            action = SimAction::Quit;
        }
        action
    }

    fn toggle_pause(&mut self) {
        match &mut self.source {
            Source::Live(h) => {
                self.paused = !self.paused;
                if self.paused {
                    h.pause();
                } else {
                    h.resume();
                }
            }
            Source::Replay(r) => {
                let c = &mut r.player.clock;
                if !c.playing {
                    // Relancer depuis une extrémité repart dans le sens de lecture.
                    if !c.reverse && c.position >= c.last() {
                        c.seek(0.0);
                    } else if c.reverse && c.position <= 0.0 {
                        let last = c.last();
                        c.seek(last);
                    }
                }
                c.playing = !c.playing;
            }
        }
    }

    fn step_once(&mut self) {
        match &mut self.source {
            Source::Live(h) => {
                if !self.paused {
                    self.paused = true;
                    h.pause();
                }
                h.step_once();
            }
            Source::Replay(r) => r.player.clock.step(1),
        }
    }

    // ------------------------------------------------------------------
    // Enregistrement
    // ------------------------------------------------------------------

    fn toggle_recording(&mut self) {
        let Source::Live(h) = &self.source else {
            self.notify(tr("Enregistrement indisponible pendant une relecture."));
            return;
        };
        if h.recording_status()
            .active
            .load(std::sync::atomic::Ordering::Acquire)
        {
            h.stop_recording();
            self.notify(tr("Enregistrement arrêté."));
            return;
        }
        if self.snapshot.phase != Phase::Ready {
            self.notify(tr("La simulation n'est pas encore prête."));
            return;
        }
        let name = h.name();
        let budget = self.record_budget_enabled.then_some(self.record_budget);
        match self
            .service
            .start_recording(&name, h, self.record_interval, budget)
        {
            Ok(()) => self.notify(tr!("Enregistrement démarré : un instantané tous les {} pas.", self.record_interval)),
            Err(e) => self.notify(tr!("Enregistrement impossible : {}", e)),
        }
    }

    fn handle_navigation(&mut self, ctx: &egui::Context, dt: f64, viewport: egui::Rect) {
        if self.menu_open {
            return;
        }
        let (moves, roll, mouse, scroll, shift, ctrl) = ctx.input(|i| {
            let k = |key: Key| if i.key_down(key) { 1.0 } else { 0.0 };
            let moves = DVec3::new(k(Key::D) - k(Key::Q), 0.0, k(Key::Z) - k(Key::S));
            let mut mouse = egui::Vec2::ZERO;
            for e in &i.events {
                if let egui::Event::MouseMoved(d) = e {
                    mouse += *d;
                }
            }
            (
                moves,
                k(Key::E) - k(Key::A),
                mouse,
                i.smooth_scroll_delta.y as f64,
                i.modifiers.shift,
                i.modifiers.ctrl,
            )
        });
        let boost = if shift {
            5.0
        } else if ctrl {
            0.2
        } else {
            1.0
        };

        // Orientation (souris capturée uniquement), dans le repère de l'écran ;
        // en suivi, la caméra tourne autour de la cible. Le premier mouvement
        // suivant une (re)capture est ignoré : il peut contenir un saut parasite.
        let just_captured = self.capture_applied != Some(true);
        if self.captured && !just_captured && mouse != egui::Vec2::ZERO {
            self.camera.rotate(
                -(mouse.x as f64) * MOUSE_SENSITIVITY,
                -(mouse.y as f64) * MOUSE_SENSITIVITY,
            );
        }

        // A / E : bascule autour de l'axe de visée (interrompt le redressement).
        if roll != 0.0 {
            self.leveling = false;
            let k = if shift {
                2.0
            } else if ctrl {
                0.3
            } else {
                1.0
            };
            self.camera.roll(roll * ROLL_SPEED * k * dt);
        }

        // Redressement progressif de l'horizon (H).
        if self.leveling {
            match self.camera.roll_to(self.reference_up()) {
                Some(a) if a.abs() > 2e-3 => {
                    self.camera.roll(a * (1.0 - (-LEVEL_RATE * dt).exp()));
                }
                Some(a) => {
                    self.camera.roll(a);
                    self.leveling = false;
                }
                None => self.leveling = false,
            }
        }

        // Déplacement : ZQSD en vol libre ; en suivi, Z / S règlent la distance
        // à la cible (Q / D sont sans effet). Vitesse proportionnelle à la
        // distance à la référence.
        let r_target = self
            .cache
            .selected_index
            .map(|i| body_radius(self.snapshot.state.bodies.masses[i], self.base_radius()))
            .unwrap_or(0.0);
        let rate = self.camera.speed * boost;
        let v = self.free_speed() * boost;
        match &mut self.camera.mode {
            Mode::Free => {
                if moves != DVec3::ZERO {
                    self.camera.translate_local(moves.normalize() * v * dt);
                }
            }
            Mode::Orbit { distance, .. } => {
                if moves.z != 0.0 {
                    let k = (-moves.z * rate * dt).exp();
                    *distance = (*distance * k).max(1.5 * r_target).max(1e-9);
                }
            }
            Mode::Barycenter { distance } => {
                if moves.z != 0.0 {
                    let k = (-moves.z * rate * dt).exp();
                    *distance = (*distance * k).max(1e-9);
                }
            }
        }

        // Molette : zoom optique (champ de vision), dans tous les modes.
        let over_ui = ctx.is_pointer_over_area() && !self.captured;
        if scroll != 0.0 && !over_ui {
            let k = (-scroll * SCROLL_SENSITIVITY).exp();
            self.camera.fov_y = (self.camera.fov_y * k).clamp(MIN_FOV, MAX_FOV);
        }

        // Souris libre : un clic droit reprend la visée.
        if !self.captured && ctx.input(|i| i.pointer.secondary_clicked()) {
            self.captured = true;
            return;
        }
        self.last_aspect = (viewport.width() / viewport.height().max(1.0)) as f64;

        // Sélection au clic.
        let clicked = ctx.input(|i| i.pointer.primary_clicked());
        if clicked {
            let aspect = (viewport.width() / viewport.height().max(1.0)) as f64;
            let pointer = ctx.input(|i| i.pointer.interact_pos());
            let ray = if self.captured {
                Some(self.camera.ray(0.0, 0.0, aspect))
            } else if !ctx.wants_pointer_input() && !ctx.is_pointer_over_area() {
                pointer.filter(|p| viewport.contains(*p)).map(|p| {
                    let nx = ((p.x - viewport.left()) / viewport.width()) as f64 * 2.0 - 1.0;
                    let ny = 1.0 - ((p.y - viewport.top()) / viewport.height()) as f64 * 2.0;
                    self.camera.ray(nx, ny, aspect)
                })
            } else {
                None
            };
            if let Some((o, d)) = ray {
                let px = viewport.height() as f64;
                let tol = PICK_TOLERANCE_PX * 2.0 * (0.5 * self.camera.fov_y).tan() / px.max(1.0);
                if let Some(i) = pick(&self.snapshot.state.bodies, o, d, tol, self.base_radius()) {
                    self.select_index(i);
                }
            }
        }

        // Orbite : la caméra accompagne le corps suivi.
        if let (Mode::Orbit { .. }, Some(p)) = (self.camera.mode, self.selected_position()) {
            self.camera.place_in_orbit(p);
        }
        if let Mode::Barycenter { .. } = self.camera.mode {
            self.camera.place_in_orbit(self.cache.barycenter);
        }
    }

    // ------------------------------------------------------------------
    // Sauvegarde
    // ------------------------------------------------------------------

    fn start_save(&mut self) {
        if self.save_job.is_some() {
            return;
        }
        if self.source.live().is_none() {
            self.notify(tr("Sauvegarde indisponible pendant une relecture."));
            return;
        }
        if self.snapshot.phase != Phase::Ready {
            self.notify(tr("La simulation n'est pas encore prête."));
            return;
        }
        let (tx, rx) = mpsc::channel();
        let service = self.service.clone();
        let config = self.source.config().clone();
        let snapshot = Arc::clone(&self.snapshot);
        let view = self.camera.to_view();
        // Enregistrement en cours : la sauvegarde le mémorise pour le poursuivre.
        let recording = self.source.live().and_then(|h| {
            h.recording_status()
                .current_path
                .lock()
                .ok()
                .and_then(|p| p.clone())
        });
        let name = self
            .source
            .live()
            .map_or_else(|| "simulation".to_owned(), |h| h.name());
        std::thread::spawn(move || {
            let r = service
                .save(&name, &config, &snapshot, Some(view), recording)
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.save_job = Some(rx);
        self.notify(tr("Sauvegarde en cours…"));
    }

    fn poll_save(&mut self) {
        let Some(rx) = &self.save_job else { return };
        if let Ok(r) = rx.try_recv() {
            self.save_job = None;
            match r {
                Ok(p) => self.notify(tr!("Sauvegardé : {}", p.display())),
                Err(e) => self.notify(tr!("Échec de la sauvegarde : {}", e)),
            }
        }
    }

    // ------------------------------------------------------------------
    // Rendu
    // ------------------------------------------------------------------

    fn rebase_threshold(&self) -> f64 {
        match self.camera.mode {
            Mode::Orbit { distance, .. } | Mode::Barycenter { distance } => {
                (50.0 * distance).min(0.05 * self.reference_radius())
            }
            Mode::Free => 0.05 * self.reference_radius(),
        }
    }

    fn prepare_batch(&mut self) {
        let drift = (self.camera.position - self.anchor).length();
        if !self.need_upload && drift <= self.rebase_threshold() {
            return;
        }
        self.need_upload = false;
        self.anchor = self.camera.position;
        let bodies = &self.snapshot.state.bodies;
        let max_mass = self.snapshot.top.first().map(|t| t.1).unwrap_or(1.0);
        let r0 = self.base_radius();
        let anchor = self.anchor;
        // Poids encodé dans le canal alpha (×16) : la poussière sous-échantillonnée
        // d'un enregistrement brille d'autant plus qu'elle en représente davantage.
        let dust_alpha = (self.source.dust_weight() * 16.0).round().clamp(1.0, 255.0) as u8;
        let instances: Vec<Instance> = bodies
            .positions
            .par_iter()
            .zip(bodies.masses.par_iter())
            .map(|(p, &m)| {
                let rel = (*p - anchor).as_vec3();
                let mut color = mass_color(m, max_mass);
                color[3] = if is_dust(m) { dust_alpha } else { 16 };
                Instance {
                    pos_radius: [rel.x, rel.y, rel.z, body_radius(m, r0) as f32],
                    color,
                }
            })
            .collect();
        self.generation += 1;
        self.batch = Some(InstanceBatch {
            generation: self.generation,
            instances: Arc::new(instances),
        });
    }

    fn paint_scene(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        // Affichage direct : les positions du moteur sont déjà en f32 ; l'ancre
        // (retranchée dans le shader) suit simplement la caméra.
        let source = match &self.direct {
            Some(view) => {
                self.anchor = self.camera.position;
                PointsSource::Direct {
                    view: Arc::clone(view),
                    anchor: self.anchor,
                }
            }
            None => {
                self.prepare_batch();
                PointsSource::Batch(self.batch.clone())
            }
        };
        let max_mass = self.snapshot.top.first().map_or(1.0, |t| t.1);
        let ppp = ui.ctx().pixels_per_point();
        let aspect = rect.width() / rect.height().max(1.0);
        let a = self.anchor.as_vec3();
        let uniforms = Uniforms {
            view: self.camera.view_matrix(self.anchor).to_cols_array_2d(),
            proj: self.camera.projection(aspect).to_cols_array_2d(),
            screen: [
                rect.height() * ppp,
                (0.5 * self.camera.fov_y).tan() as f32,
                self.min_pixels,
                0.0,
            ],
            look: [self.brightness, self.dust_floor, 0.0, 0.0],
            anchor: [a.x, a.y, a.z, self.base_radius() as f32],
            mass: [
                if max_mass > 1.0 {
                    max_mass.ln() as f32
                } else {
                    0.0
                },
                0.0,
                0.0,
                0.0,
            ],
        };
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            PointsCallback { uniforms, source },
        ));
        self.paint_overlay(ui, rect);
    }

    fn paint_overlay(&self, ui: &egui::Ui, rect: egui::Rect) {
        let painter = ui.painter_at(rect);
        let aspect = (rect.width() / rect.height().max(1.0)) as f64;
        let to_screen = |(x, y): (f64, f64)| {
            Pos2::new(
                rect.center().x + (x as f32) * 0.5 * rect.width(),
                rect.center().y - (y as f32) * 0.5 * rect.height(),
            )
        };

        if self.show_grid && !self.snapshot.state.bodies.is_empty() {
            self.paint_disk_grid(&painter, aspect, &to_screen);
        }

        // Repère du barycentre sélectionné : réticule doré.
        if self.bary_selected {
            if let Some(ndc) = self.camera.project(self.cache.barycenter, aspect) {
                let c = to_screen(ndc);
                let gold = Color32::from_rgb(255, 200, 80);
                let s = Stroke::new(1.5_f32, gold);
                painter.circle_stroke(c, 11.0, s);
                for (a, b) in [
                    (egui::vec2(-20.0, 0.0), egui::vec2(-5.0, 0.0)),
                    (egui::vec2(5.0, 0.0), egui::vec2(20.0, 0.0)),
                    (egui::vec2(0.0, -20.0), egui::vec2(0.0, -5.0)),
                    (egui::vec2(0.0, 5.0), egui::vec2(0.0, 20.0)),
                ] {
                    painter.line_segment([c + a, c + b], s);
                }
                painter.circle_filled(c, 2.0, gold);
                painter.text(
                    c + egui::vec2(0.0, 26.0),
                    egui::Align2::CENTER_TOP,
                    "barycentre",
                    egui::FontId::proportional(12.0),
                    gold,
                );
            }
        }

        // Repère du corps sélectionné.
        if let (Some(i), Some(p)) = (self.cache.selected_index, self.selected_position()) {
            let b = &self.snapshot.state.bodies;
            if let Some(ndc) = self.camera.project(p, aspect) {
                let c = to_screen(ndc);
                let d = (p - self.camera.position).length().max(1e-12);
                let r_world = body_radius(b.masses[i], self.base_radius());
                let r_px = (r_world / (d * (0.5 * self.camera.fov_y).tan())
                    * 0.5
                    * rect.height() as f64) as f32;
                let ring = r_px.max(6.0) + 6.0;
                painter.circle_stroke(
                    c,
                    ring,
                    Stroke::new(1.5_f32, Color32::from_rgb(120, 220, 255)),
                );
            }
        }

        // Cibles hors champ : flèche au bord de l'écran et distance.
        if !self.snapshot.state.bodies.is_empty() {
            let gold = Color32::from_rgb(255, 200, 80);
            self.paint_edge_marker(
                &painter,
                rect,
                aspect,
                self.cache.barycenter,
                "barycentre",
                gold,
            );
        }
        if let (Some(id), Some(p)) = (self.selected, self.selected_position()) {
            if !self.bary_selected {
                let label = format!("#{id}");
                let blue = Color32::from_rgb(120, 220, 255);
                self.paint_edge_marker(&painter, rect, aspect, p, &label, blue);
            }
        }
        self.paint_triad(&painter, rect);

        // Traces.
        if self.traces_active {
            // Copie rapide sous verrou, puis dessin hors verrou.
            let lines: Vec<Vec<Option<DVec3>>> = self.source.with_traces(|set| match set {
                Some(set) => {
                    let reference = match self.trace_reference() {
                        Some(TraceReference::Body(id)) => set.reference_samples(id),
                        Some(r) => set.samples_for(r),
                        None => None,
                    };
                    set.traces
                        .iter()
                        .map(|t| {
                            let pts = if self.trace_tail_only {
                                t.recent(self.trace_tail_len)
                            } else {
                                t.full()
                            };
                            match &reference {
                                Some(r) => relative_to(&pts, r),
                                None => pts.into_iter().map(|(_, p)| Some(p)).collect(),
                            }
                        })
                        .collect()
                }
                None => Vec::new(),
            });
            for (k, pts) in lines.iter().enumerate() {
                let color = TRACE_PALETTE[k % TRACE_PALETTE.len()];
                if pts.len() < 2 {
                    continue;
                }
                let screen: Vec<Option<Pos2>> = pts
                    .iter()
                    .map(|p| {
                        p.and_then(|p| self.camera.project(p, aspect))
                            .map(to_screen)
                    })
                    .collect();
                if self.trace_tail_only {
                    // Dégradé : opaque au présent, transparent vers le passé.
                    let n = screen.len() as f32;
                    for (k, w) in screen.windows(2).enumerate() {
                        if let [Some(a), Some(b)] = w {
                            let alpha = (k as f32 + 1.0) / n;
                            painter.line_segment(
                                [*a, *b],
                                Stroke::new(1.5_f32, color.gamma_multiply(alpha)),
                            );
                        }
                    }
                } else {
                    // Tronçons continus devant la caméra, en une seule polyligne chacun.
                    let mut run: Vec<Pos2> = Vec::new();
                    for p in screen.iter().copied().chain(std::iter::once(None)) {
                        match p {
                            Some(q) => run.push(q),
                            None => {
                                if run.len() >= 2 {
                                    painter.add(egui::Shape::line(
                                        std::mem::take(&mut run),
                                        Stroke::new(1.5_f32, color),
                                    ));
                                }
                                run.clear();
                            }
                        }
                    }
                }
            }
        }

        // Réticule en mode capturé.
        if self.captured && !self.menu_open {
            let c = rect.center();
            let col = Color32::from_white_alpha(170);
            let s = Stroke::new(1.2_f32, col);
            painter.line_segment([c - egui::vec2(9.0, 0.0), c - egui::vec2(3.0, 0.0)], s);
            painter.line_segment([c + egui::vec2(3.0, 0.0), c + egui::vec2(9.0, 0.0)], s);
            painter.line_segment([c - egui::vec2(0.0, 9.0), c - egui::vec2(0.0, 3.0)], s);
            painter.line_segment([c + egui::vec2(0.0, 3.0), c + egui::vec2(0.0, 9.0)], s);
        }

        if self.snapshot.phase == Phase::Initialising {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr!("Génération de {} particules et premier calcul des forces…", format::count(self.source.config().initial.count as u64)),
                egui::FontId::proportional(22.0),
                Color32::from_gray(220),
            );
        }
        if self.snapshot.phase == Phase::Failed {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr!("Le moteur de calcul n'a pas pu démarrer :\n{}\n\nÉchap → Retour à l'accueil, puis choisir un autre moteur dans les paramètres avancés.", self.snapshot.error.as_deref().unwrap_or("erreur inconnue")),
                egui::FontId::proportional(18.0),
                Color32::from_rgb(255, 140, 120),
            );
        }

        if let Some((msg, t)) = &self.toast {
            if t.elapsed() < TOAST_DURATION {
                painter.text(
                    rect.center_bottom() - egui::vec2(0.0, 44.0),
                    egui::Align2::CENTER_CENTER,
                    msg,
                    egui::FontId::proportional(16.0),
                    Color32::from_rgb(255, 210, 140),
                );
            }
        }

        let hint = if self.captured {
            tr("Tab : libérer la souris · Échap : menu · F : suivre / cesser · A / E : bascule · H : horizon · G : grille · C : barycentre · V : vue d'ensemble · T : traces")
        } else {
            tr("Souris libre — clic gauche : suivre un corps · clic droit ou Tab : reprendre la visée · Échap : menu")
        };
        painter.text(
            rect.center_bottom() - egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            hint,
            egui::FontId::proportional(13.0),
            Color32::from_gray(150),
        );
    }

    /// Flèche au bord de l'écran vers un point hors champ, avec sa distance.
    fn paint_edge_marker(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        aspect: f64,
        p: DVec3,
        label: &str,
        color: Color32,
    ) {
        let l = self.camera.to_local(p);
        if let Some((x, y)) = self.camera.project_local(l, aspect) {
            if x.abs() <= 1.0 && y.abs() <= 1.0 {
                return;
            }
        }
        // Direction à l'écran (devant comme derrière la caméra : le sens
        // dans lequel tourner pour l'atteindre).
        let t = (0.5 * self.camera.fov_y).tan();
        let sx = l.x / (t * aspect) * 0.5 * rect.width() as f64;
        let sy = -l.y / t * 0.5 * rect.height() as f64;
        let dir = egui::vec2(sx as f32, sy as f32);
        let dir = if dir.length() > 1e-6 {
            dir.normalized()
        } else {
            egui::vec2(0.0, 1.0)
        };
        let margin = 30.0;
        let hw = (0.5 * rect.width() - margin).max(1.0);
        let hh = (0.5 * rect.height() - margin).max(1.0);
        let scale = (hw / dir.x.abs().max(1e-6)).min(hh / dir.y.abs().max(1e-6));
        let pos = rect.center() + dir * scale;
        let perp = egui::vec2(-dir.y, dir.x);
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos + dir * 12.0,
                pos - dir * 6.0 + perp * 8.0,
                pos - dir * 6.0 - perp * 8.0,
            ],
            color.gamma_multiply(0.85),
            Stroke::NONE,
        ));
        let d = (p - self.camera.position).length();
        painter.text(
            pos - dir * 18.0,
            {
                // Le texte s'étend vers le centre de l'écran.
                let h = if dir.x > 0.3 {
                    egui::Align::Max
                } else if dir.x < -0.3 {
                    egui::Align::Min
                } else {
                    egui::Align::Center
                };
                let v = if dir.y > 0.3 {
                    egui::Align::Max
                } else if dir.y < -0.3 {
                    egui::Align::Min
                } else {
                    egui::Align::Center
                };
                egui::Align2([h, v])
            },
            format!("{label}\n{}", format::real(d)),
            egui::FontId::proportional(12.0),
            color,
        );
    }

    /// Trièdre d'orientation (coin inférieur gauche) : axes X, Y, Z du monde
    /// et normale au disque.
    fn paint_triad(&self, painter: &egui::Painter, rect: egui::Rect) {
        let center = rect.left_bottom() + egui::vec2(58.0, -78.0);
        let size = 34.0_f32;
        painter.circle_filled(center, size + 12.0, Color32::from_black_alpha(110));
        let mut axes: Vec<(DVec3, &str, Color32)> = vec![
            (DVec3::X, "X", Color32::from_rgb(235, 90, 80)),
            (DVec3::Y, "Y", Color32::from_rgb(110, 220, 100)),
            (DVec3::Z, "Z", Color32::from_rgb(90, 150, 255)),
        ];
        if let Some(n) = self.disk_normal {
            axes.push((n, "N", Color32::from_rgb(255, 200, 80)));
        }
        let (r, u, f) = (self.camera.right(), self.camera.up(), self.camera.forward());
        // Du plus lointain (vers l'écran) au plus proche.
        axes.sort_by(|a, b| b.0.dot(f).total_cmp(&a.0.dot(f)));
        for (v, name, color) in axes {
            let depth = v.dot(f) as f32; // > 0 : s'enfonce dans l'écran
            let color = color.gamma_multiply(1.0 - 0.55 * depth.max(0.0));
            let end = center + egui::vec2(v.dot(r) as f32, -(v.dot(u) as f32)) * size;
            let width: f32 = if name == "N" { 2.5 } else { 2.0 };
            painter.line_segment([center, end], Stroke::new(width, color));
            painter.circle_filled(end, 2.5, color);
            let off = (end - center).normalized() * 9.0;
            painter.text(
                end + if off.is_finite() {
                    off
                } else {
                    egui::Vec2::ZERO
                },
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(11.0),
                color,
            );
        }
    }

    /// Grille du plan du disque (passant par le barycentre, perpendiculaire à
    /// la normale au disque), estompée avec la distance au pied de la caméra.
    fn paint_disk_grid(
        &self,
        painter: &egui::Painter,
        aspect: f64,
        to_screen: &dyn Fn((f64, f64)) -> Pos2,
    ) {
        let n = self.reference_up().normalize();
        let o = self.cache.barycenter;
        let cam = &self.camera;
        let h = (cam.position - o).dot(n);
        let height = h.abs().max(0.02 * self.reference_radius());
        // Maille : puissance de 10 comprise entre height/10 et height.
        let step = 10f64.powf(height.log10().floor());
        let (u, v) = n.any_orthonormal_pair();
        let foot = cam.position - n * h;
        let fu = (foot - o).dot(u) / step;
        let fv = (foot - o).dot(v) / step;
        let fade = GRID_HALF_LINES as f64 * step;
        let near = 1e-3 * height;
        let base = Color32::from_rgb(110, 150, 220);
        for (a, b, fa, fb) in [(u, v, fu, fv), (v, u, fv, fu)] {
            let i0 = fa.round() as i64;
            let j0 = fb.round() as i64;
            for i in (i0 - GRID_HALF_LINES)..=(i0 + GRID_HALF_LINES) {
                let major = i.rem_euclid(10) == 0;
                let strength = if major { 0.45 } else { 0.2 };
                let t0 = (j0 - GRID_HALF_LINES) as f64;
                let t1 = (j0 + GRID_HALF_LINES) as f64;
                let point = |t: f64| o + a * (i as f64 * step) + b * (t * step);
                for k in 0..GRID_SUBDIVISIONS {
                    let ta = t0 + (t1 - t0) * k as f64 / GRID_SUBDIVISIONS as f64;
                    let tb = t0 + (t1 - t0) * (k + 1) as f64 / GRID_SUBDIVISIONS as f64;
                    let (pa, pb) = (point(ta), point(tb));
                    let mid = 0.5 * (pa + pb);
                    let d = (mid - foot).length() / fade;
                    let alpha = ((1.0 - d).max(0.0).powi(2) * strength) as f32;
                    if alpha < 0.01 {
                        continue;
                    }
                    let (mut la, mut lb) = (cam.to_local(pa), cam.to_local(pb));
                    if la.z < near && lb.z < near {
                        continue;
                    }
                    if la.z < near {
                        la += (lb - la) * ((near - la.z) / (lb.z - la.z));
                    } else if lb.z < near {
                        lb += (la - lb) * ((near - lb.z) / (la.z - lb.z));
                    }
                    if let (Some(sa), Some(sb)) =
                        (cam.project_local(la, aspect), cam.project_local(lb, aspect))
                    {
                        painter.line_segment(
                            [to_screen(sa), to_screen(sb)],
                            Stroke::new(1.0_f32, base.gamma_multiply(alpha)),
                        );
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // HUD
    // ------------------------------------------------------------------

    fn hud(&mut self, ctx: &egui::Context) {
        let snap = Arc::clone(&self.snapshot);
        let title = if self.source.replay().is_some() {
            tr("Relecture")
        } else {
            tr("Simulation")
        };
        egui::Window::new(title)
            .id(egui::Id::new("panneau-simulation"))
            .anchor(egui::Align2::LEFT_TOP, [10.0, 10.0])
            .resizable(false)
            .default_width(280.0)
            .show(ctx, |ui| {
                if self.source.replay().is_some() {
                    self.replay_panel(ui);
                } else {
                    self.live_panel(ui, &snap);
                }
                ui.separator();
                self.view_settings(ui);
            });

        let mut clicked_id: Option<BodyId> = None;
        egui::Window::new(tr("Corps"))
            .anchor(egui::Align2::RIGHT_TOP, [-10.0, 10.0])
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                if self.bary_selected {
                    ui.label(RichText::new(tr("Barycentre sélectionné")).strong());
                    egui::Grid::new("bary")
                        .num_columns(2)
                        .spacing([14.0, 3.0])
                        .show(ui, |ui| {
                            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                ui.label(k);
                                ui.label(RichText::new(v).monospace());
                                ui.end_row();
                            };
                            let c = self.cache.barycenter;
                            row(ui, tr("Position"), format!("{} ; {} ; {}", format::real(c.x), format::real(c.y), format::real(c.z)));
                            row(ui, tr("Masse présente"), format::real(snap.total_mass));
                            row(ui, tr("Vitesse"), format::real(self.cache.barycenter_velocity.length()));
                            row(ui, tr("Distance caméra"), format::real((c - self.camera.position).length()));
                            match self.cache.rotation_axis {
                                Some(a) => row(
                                    ui,
                                    tr("Axe de rotation"),
                                    tr!("{}° sur Oz", format!("{:.1}", a.angle_between(DVec3::Z).to_degrees())).replace('.', crate::format::decimal_sep()),
                                ),
                                None => row(ui, tr("Axe de rotation"), tr("aucune rotation").to_owned()),
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui.button(tr("Vue de face (B)")).clicked() {
                            self.view_barycenter_from_axis();
                        }
                        let label = if self.camera.is_orbiting() { tr("Cesser le suivi (F)") } else { tr("Suivre (F)") };
                        if ui.button(label).clicked() {
                            self.toggle_follow();
                        }
                        if ui.button(tr("Désélectionner")).clicked() {
                            self.bary_selected = false;
                            self.camera.mode = Mode::Free;
                        }
                    });
                } else {
                ui.label(RichText::new(tr("Corps sélectionné")).strong());
                match self.cache.selected_index {
                    Some(i) => {
                        let b = &snap.state.bodies;
                        let m = b.masses[i];
                        let p = self.selected_position().unwrap_or(b.positions[i]);
                        egui::Grid::new("sel")
                            .num_columns(2)
                            .spacing([14.0, 3.0])
                            .show(ui, |ui| {
                                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                    ui.label(k);
                                    ui.label(RichText::new(v).monospace());
                                    ui.end_row();
                                };
                                row(ui, tr("Identifiant"), b.ids[i].to_string());
                                if let Some(r) = self.cache.selected_rank {
                                    row(
                                        ui,
                                        tr("Rang (masse)"),
                                        format!(
                                            "{} / {}",
                                            format::count(r as u64),
                                            format::count(b.len() as u64)
                                        ),
                                    );
                                }
                                row(ui, tr("Masse"), format::real(m));
                                row(
                                    ui,
                                    tr("Rayon"),
                                    format::real(body_radius(m, self.base_radius())),
                                );
                                row(ui, tr("Vitesse"), format::real(b.velocities[i].length()));
                                row(
                                    ui,
                                    tr("Dist. barycentre"),
                                    format::real((p - self.cache.barycenter).length()),
                                );
                            });
                        ui.horizontal(|ui| {
                            let label = if self.camera.is_orbiting() { tr("Cesser le suivi (F)") } else { tr("Suivre (F)") };
                            if ui.button(label).clicked() {
                                self.toggle_follow();
                            }
                            if ui.button(tr("Désélectionner")).clicked() {
                                self.selected = None;
                                self.camera.mode = Mode::Free;
                                self.cache.selected_index = None;
                            }
                        });
                    }
                    None => {
                        ui.label(
                            RichText::new(tr("Aucun — visez et cliquez, ou touches Origine ou B (barycentre).")).weak(),
                        );
                    }
                }
                }
                ui.separator();
                self.traces_panel(ui);
                ui.separator();
                ui.label(RichText::new(tr("Les plus massifs")).strong());
                egui::Grid::new("top")
                    .num_columns(4)
                    .spacing([10.0, 2.0])
                    .show(ui, |ui| {
                        for (rank, (id, m)) in snap.top.iter().enumerate() {
                            ui.label(format!("{}.", rank + 1));
                            let sel = self.selected == Some(*id);
                            if ui.selectable_label(sel, format!("#{id}")).clicked() {
                                clicked_id = Some(*id);
                            }
                            ui.label(RichText::new(format::real(*m)).monospace());
                            ui.label(
                                RichText::new(format_share(snap.mass_fraction(*m)))
                                    .monospace()
                                    .weak(),
                            );
                            ui.end_row();
                        }
                        // Corps sélectionné hors du classement : son rang et
                        // ses grandeurs, à la suite.
                        let outside = self
                            .selected
                            .filter(|id| !self.bary_selected && !snap.top.iter().any(|t| t.0 == *id));
                        if let (Some(id), Some(i), Some(rank)) =
                            (outside, self.cache.selected_index, self.cache.selected_rank)
                        {
                            let m = snap.state.bodies.masses[i];
                            ui.label(RichText::new("…").weak());
                            ui.end_row();
                            ui.label(format!("{}.", format::count(rank as u64)));
                            let _ = ui.selectable_label(true, format!("#{id}"));
                            ui.label(RichText::new(format::real(m)).monospace());
                            ui.label(
                                RichText::new(format_share(snap.mass_fraction(m)))
                                    .monospace()
                                    .weak(),
                            );
                            ui.end_row();
                        }
                    });
                if self.source.replay().is_none() {
                    ui.separator();
                    if let Some(id) = self.clusters_panel(ui, &snap) {
                        clicked_id = Some(id);
                    }
                }
            });
        if let Some(id) = clicked_id {
            if let Some(i) = self.snapshot.state.bodies.index_of(id) {
                self.select_index(i);
            }
        }
    }

    /// Amas éjectés conservés par le nettoyage ; renvoie le corps repère
    /// de l'amas cliqué.
    fn clusters_panel(&self, ui: &mut egui::Ui, snap: &Snapshot) -> Option<BodyId> {
        /// Amas affichés au plus.
        const SHOWN: usize = 10;
        let mut clicked = None;
        if snap.systems.len() > 1 {
            ui.label(RichText::new(tr("Sous-simulations")).strong()).on_hover_text(
                tr("Amas éjectés détachés : chacun est calculé à part, dans son propre repère, et ressent les autres par leur champ de marée. Deux sous-simulations qui se rapprochent sont réunies."),
            );
            egui::Grid::new("systems")
                .num_columns(4)
                .spacing([10.0, 2.0])
                .show(ui, |ui| {
                    ui.label(RichText::new(tr("Repère")).weak());
                    ui.label(RichText::new(tr("Corps")).weak());
                    ui.label(RichText::new(tr("Masse")).weak());
                    ui.label(RichText::new(tr("Distance")).weak());
                    ui.end_row();
                    for s in &snap.systems {
                        let sel = self.selected == Some(s.anchor);
                        if ui
                            .selectable_label(sel, format!("#{}", s.anchor))
                            .on_hover_text(tr!("Taille : {}
Vitesse : {}", format::real(s.size), format::real(s.velocity.length())))
                            .clicked()
                        {
                            clicked = Some(s.anchor);
                        }
                        ui.label(RichText::new(format::count(s.bodies as u64)).monospace());
                        ui.label(RichText::new(format::real(s.mass)).monospace());
                        ui.label(
                            RichText::new(format::real((s.center - self.cache.barycenter).length()))
                                .monospace(),
                        );
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
        }
        ui.label(RichText::new(tr("Amas éjectés")).strong())
            .on_hover_text(
                tr("Groupes liés de corps lointains, conservés par le nettoyage (seuils dans les paramètres avancés). Cliquer sur un amas sélectionne son corps le plus massif."),
            );
        let Some(step) = snap.clusters_step else {
            ui.label(RichText::new(tr("Aucun nettoyage effectué pour l'instant.")).weak());
            return None;
        };
        if snap.clusters.is_empty() {
            ui.label(
                RichText::new(tr!("Aucun au dernier nettoyage (pas {}).", format::count(step)))
                    .weak(),
            );
            return None;
        }
        ui.label(
            RichText::new(tr!("Au dernier nettoyage (pas {}) :", format::count(step))).weak(),
        );
        egui::Grid::new("clusters")
            .num_columns(4)
            .spacing([10.0, 2.0])
            .show(ui, |ui| {
                ui.label(RichText::new(tr("Repère")).weak());
                ui.label(RichText::new(tr("Corps")).weak());
                ui.label(RichText::new(tr("Masse")).weak());
                ui.label(RichText::new(tr("Distance")).weak());
                ui.end_row();
                for c in snap.clusters.iter().take(SHOWN) {
                    let sel = self.selected == Some(c.anchor);
                    if ui
                        .selectable_label(sel, format!("#{}", c.anchor))
                        .on_hover_text(tr!("Rayon de demi-masse : {}
Vitesse : {}", format::real(c.half_mass_radius), format::real(c.velocity.length())))
                        .clicked()
                    {
                        clicked = Some(c.anchor);
                    }
                    ui.label(RichText::new(format::count(c.bodies as u64)).monospace());
                    ui.label(RichText::new(format::real(c.mass)).monospace());
                    ui.label(
                        RichText::new(format::real((c.center - self.cache.barycenter).length()))
                            .monospace(),
                    );
                    ui.end_row();
                }
            });
        let hidden = snap.clusters.len().saturating_sub(SHOWN);
        if hidden > 0 {
            ui.label(RichText::new(tr!("… et {} autres", format::count(hidden as u64))).weak());
        }
        clicked
    }

    /// Masses des corps tracés, tirées de l'état complet (un passage).
    fn refresh_trace_masses(&mut self) {
        self.cache.trace_masses.clear();
        if !self.traces_active {
            return;
        }
        let ids: std::collections::HashSet<BodyId> = self.source.with_traces(|set| {
            set.map(|s| s.traces.iter().filter_map(|t| t.current).collect())
                .unwrap_or_default()
        });
        if ids.is_empty() {
            return;
        }
        let b = &self.snapshot.state.bodies;
        for (i, id) in b.ids.iter().enumerate() {
            if ids.contains(id) {
                self.cache.trace_masses.insert(*id, b.masses[i]);
            }
        }
    }

    fn traces_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Traces (T)")).strong());
            let label = if self.traces_active {
                tr("Effacer")
            } else {
                tr("Tracer les 10 plus gros visibles")
            };
            if ui.small_button(label).clicked() {
                self.toggle_traces();
            }
        });
        if !self.traces_active {
            return;
        }
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.trace_tail_only, false, tr("Historique"));
            ui.radio_value(&mut self.trace_tail_only, true, tr("Queue"));
        });
        ui.horizontal(|ui| {
            ui.label(tr("Référentiel (R)"));
            for frame in [TraceFrame::Body, TraceFrame::Barycenter, TraceFrame::Cluster] {
                ui.radio_value(&mut self.trace_frame, frame, frame.label());
            }
        });
        let reference = self.trace_reference();
        let hint = match (self.trace_frame, reference) {
            (TraceFrame::Barycenter, _) => tr("Orbites vues depuis le centre de gravité de la simulation.").to_owned(),
            (_, Some(TraceReference::Barycenter)) => {
                tr("Aucun corps sélectionné : traces relatives au barycentre en attendant.").to_owned()
            }
            (TraceFrame::Cluster, _) => {
                let n = self.source.with_traces(|set| set.map_or(0, |s| s.cluster_size()));
                tr!("Centre de gravité de l'amas lié du corps sélectionné ({} corps suivis).", format::count(n as u64))
            }
            _ => tr("Orbites vues depuis le corps sélectionné.").to_owned(),
        };
        ui.label(RichText::new(hint).weak());
        if self.trace_tail_only {
            ui.add(
                egui::Slider::new(&mut self.trace_tail_len, 10..=MAX_TAIL)
                    .logarithmic(true)
                    .text(tr("pas")),
            );
        }
        let rows: Vec<(BodyId, Option<BodyId>)> = self.source.with_traces(|set| match set {
            Some(set) => set.traces.iter().map(|t| (t.origin, t.current)).collect(),
            None => Vec::new(),
        });
        if self.cache.trace_masses.is_empty() && !rows.is_empty() {
            self.refresh_trace_masses();
        }
        let total = self.snapshot.total_mass;
        egui::Grid::new("traces")
            .num_columns(4)
            .spacing([8.0, 1.0])
            .show(ui, |ui| {
                for (k, (origin, current)) in rows.into_iter().enumerate() {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, 10.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 2.0, TRACE_PALETTE[k % TRACE_PALETTE.len()]);
                    let text = match current {
                        Some(c) if c == origin => format!("#{c}"),
                        Some(c) => tr!("#{} → #{}", origin, c),
                        None => tr!("#{} (éliminé)", origin),
                    };
                    ui.label(RichText::new(text).monospace());
                    match current.and_then(|c| self.cache.trace_masses.get(&c)) {
                        Some(&m) => {
                            ui.label(RichText::new(format::real(m)).monospace());
                            let share = if total > 0.0 { m / total } else { 0.0 };
                            ui.label(RichText::new(format_share(share)).monospace().weak());
                        }
                        None => {
                            ui.label("");
                            ui.label("");
                        }
                    }
                    ui.end_row();
                }
            });
    }

    fn live_panel(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let engine_id = self.source.config().engine.clone();
        let engine = if engine_id.starts_with("gpu") {
            "GPU"
        } else {
            "CPU"
        };
        egui::Grid::new("stats")
            .num_columns(2)
            .spacing([14.0, 3.0])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(k);
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                };
                let state = if snap.phase == Phase::Failed {
                    tr("échec du moteur")
                } else if snap.phase == Phase::Initialising {
                    "initialisation"
                } else if self.paused {
                    tr("en pause")
                } else {
                    tr("en cours")
                };
                row(ui, tr("État"), state.to_owned());
                row(ui, tr("Moteur"), engine.to_owned());
                row(ui, tr("Corps"), format::count(snap.body_count as u64));
                row(ui, tr("Pas"), format::count(snap.step));
                row(ui, tr("Temps simulé"), format::real(snap.time));
                row(
                    ui,
                    tr("Pas / s"),
                    format!("{:.2}", snap.steps_per_second).replace('.', crate::format::decimal_sep()),
                );
                row(
                    ui,
                    tr("Durée d'un pas"),
                    format!("{:.1} ms", snap.last_step_ms).replace('.', crate::format::decimal_sep()),
                );
                row(ui, tr("Images / s"), format!("{:.0}", self.fps));
                row(
                    ui,
                    tr("Énergie totale"),
                    format::real(snap.diagnostics.total_energy()),
                );
                if let Some(d) = snap.energy_drift() {
                    row(ui, tr("ΔE / E₀"), format::percent(d));
                }
                row(ui, tr("Masse présente"), format::real(snap.total_mass));
                if let Some(e) = snap.ejected_mass() {
                    let m0 = snap.state.initial_mass.unwrap_or(0.0);
                    row(
                        ui,
                        tr("Masse éjectée"),
                        format!(
                            "{} ({})",
                            format::real(e),
                            format_share(if m0 > 0.0 { e / m0 } else { 0.0 })
                        ),
                    );
                }
            });
        ui.label(
            RichText::new(tr("ΔE inclut les pertes par fusion et éjection."))
                .small()
                .weak(),
        );
        ui.separator();
        ui.horizontal(|ui| {
            let label = if self.paused {
                tr("▶ Reprendre")
            } else {
                tr("⏸ Pause")
            };
            if ui.button(label).clicked() {
                self.toggle_pause();
            }
            if ui.button(tr("Pas à pas")).clicked() {
                self.step_once();
            }
        });
        ui.horizontal(|ui| {
            let mut changed = ui.checkbox(&mut self.limit_rate, tr("Limiter à")).changed();
            changed |= ui
                .add_enabled(
                    self.limit_rate,
                    egui::DragValue::new(&mut self.max_rate)
                        .range(0.1..=10_000.0)
                        .speed(0.5)
                        .suffix(tr(" pas/s")),
                )
                .changed();
            if changed {
                if let Some(h) = self.source.live() {
                    h.set_max_rate(self.limit_rate.then_some(self.max_rate));
                }
            }
        });
        ui.separator();
        self.recording_panel(ui);
    }

    fn view_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(tr("Luminosité"));
            ui.add(egui::Slider::new(&mut self.brightness, 0.02..=3.0).logarithmic(true));
        });
        ui.horizontal(|ui| {
            ui.label(tr("Poussières"));
            ui.add(egui::Slider::new(&mut self.dust_floor, 0.0..=1.0));
        });
        ui.horizontal(|ui| {
            ui.label(tr("Vitesse caméra"));
            let sp = self.camera.speed * 0.02;
            ui.add(
                egui::DragValue::new(&mut self.camera.speed)
                    .speed(sp)
                    .range(1e-3..=100.0)
                    .suffix(tr(" × dist./s")),
            )
            .on_hover_text(
                tr("Fraction de la distance au corps sélectionné (ou au barycentre) parcourue chaque seconde."),
            );
            ui.label(RichText::new(tr("(+/−, Maj, Ctrl)")).weak());
        });
        if ui
            .checkbox(&mut self.show_grid, tr("Grille du plan du disque (G)"))
            .changed()
            && self.show_grid
        {
            self.refresh_disk_normal(true);
        }
    }

    fn recording_panel(&mut self, ui: &mut egui::Ui) {
        use std::sync::atomic::Ordering;
        let Some(h) = self.source.live() else { return };
        let st = h.recording_status();
        let active = st.active.load(Ordering::Acquire);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Enregistrement (F9)")).strong());
            if active {
                ui.colored_label(
                    Color32::from_rgb(255, 80, 80),
                    RichText::new(tr("REC")).strong(),
                );
            }
        });
        let toggle = if active {
            let frames = st.frames.load(Ordering::Relaxed);
            let bytes = st.bytes.load(Ordering::Relaxed);
            ui.label(tr!("{} instantanés · {} · 1 tous les {} pas", format::count(frames), format_bytes(bytes), st.interval.load(Ordering::Relaxed)));
            ui.button(tr("■ Arrêter")).clicked()
        } else {
            ui.horizontal(|ui| {
                ui.label(tr("Un instantané tous les"));
                ui.add(egui::DragValue::new(&mut self.record_interval).range(1..=100_000));
                ui.label(tr("pas"));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.record_budget_enabled, tr("Poussière limitée à"));
                ui.add_enabled(
                    self.record_budget_enabled,
                    egui::DragValue::new(&mut self.record_budget)
                        .range(1_000..=100_000_000)
                        .speed(10_000.0),
                );
            });
            if let Some(e) = st.error.lock().ok().and_then(|g| g.clone()) {
                ui.colored_label(Color32::LIGHT_RED, e);
            } else if let Some(p) = st.last_path.lock().ok().and_then(|g| g.clone()) {
                ui.label(
                    RichText::new(tr!("Dernier : {}", p.display()))
                        .small()
                        .weak(),
                );
            }
            ui.button(tr("Enregistrer")).clicked()
        };
        if toggle {
            self.toggle_recording();
        }
    }

    fn replay_panel(&mut self, ui: &mut egui::Ui) {
        let fps = self.fps;
        let snap = Arc::clone(&self.snapshot);
        let Some(r) = self.source.replay_mut() else {
            return;
        };
        let n = r.player.frame_count();
        let waiting = r.player.is_waiting();
        let error = r.player.error();
        let info_interval = r.player.info().header.interval;
        let budget = r.player.info().header.dust_budget;
        let weight = r.player.dust_weight();
        let c = &mut r.player.clock;
        egui::Grid::new("replay-stats")
            .num_columns(2)
            .spacing([14.0, 3.0])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(k);
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                };
                let state = if waiting && c.playing {
                    tr("chargement…")
                } else if c.playing {
                    if c.reverse {
                        tr("lecture arrière")
                    } else {
                        "lecture"
                    }
                } else {
                    tr("en pause")
                };
                row(ui, tr("État"), state.to_owned());
                row(
                    ui,
                    tr("Instantané"),
                    format!("{:.1} / {}", c.position + 1.0, n).replace('.', crate::format::decimal_sep()),
                );
                row(ui, tr("Pas"), format::count(snap.step));
                row(ui, tr("Temps simulé"), format::real(snap.time));
                row(ui, tr("Corps affichés"), format::count(snap.body_count as u64));
                if let Some(e) = snap.ejected_mass() {
                    let m0 = snap.state.initial_mass.unwrap_or(0.0);
                    row(
                        ui,
                        tr("Masse éjectée"),
                        format!(
                            "{} ({})",
                            format::real(e),
                            format_share(if m0 > 0.0 { e / m0 } else { 0.0 })
                        ),
                    );
                }
                if weight > 1.0 {
                    row(
                        ui,
                        tr("Poussière"),
                        tr!("1 sur {}", format!("{:.1}", weight)).replace('.', crate::format::decimal_sep()),
                    );
                }
                row(ui, tr("Images / s"), format!("{fps:.0}"));
                row(ui, tr("Intervalle"), format!("{info_interval} pas"));
                if let Some(b) = budget {
                    row(ui, tr("Budget poussière"), format::count(b));
                }
            });
        if let Some(e) = error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        let mut pos = c.position;
        let last = c.last();
        let slider = ui.add(
            egui::Slider::new(&mut pos, 0.0..=last.max(1e-9))
                .show_value(false)
                .text(tr("temps")),
        );
        if slider.changed() {
            c.seek(pos);
        }
        let mut toggle = false;
        ui.horizontal(|ui| {
            if ui.button("⏮").on_hover_text(tr("Début")).clicked() {
                c.seek(0.0);
            }
            if ui
                .button("◀|")
                .on_hover_text(tr("Instantané précédent (←)"))
                .clicked()
            {
                c.step(-1);
            }
            let label = if c.playing { "⏸" } else { "▶" };
            if ui
                .button(label)
                .on_hover_text(tr("Lecture / pause (Espace)"))
                .clicked()
            {
                toggle = true;
            }
            if ui
                .button("|▶")
                .on_hover_text(tr("Instantané suivant (→)"))
                .clicked()
            {
                c.step(1);
            }
            if ui.button("⏭").on_hover_text(tr("Fin")).clicked() {
                c.seek(last);
            }
            ui.checkbox(&mut c.reverse, tr("Arrière"))
                .on_hover_text(tr("Retour arrière (⌫)"));
        });
        ui.horizontal(|ui| {
            ui.label(tr("Vitesse"));
            ui.add(
                egui::Slider::new(&mut c.speed, 0.05..=1000.0)
                    .logarithmic(true)
                    .suffix(tr(" inst./s")),
            );
        });
        ui.label(
            RichText::new(tr("↑ / ↓ : vitesse · ← / → : instantané · ⌫ : sens"))
                .small()
                .weak(),
        );
        if toggle {
            self.toggle_pause();
        }
    }

    fn menu(&mut self, ctx: &egui::Context) -> SimAction {
        let mut action = SimAction::None;
        if !self.menu_open {
            return action;
        }
        egui::Window::new(tr("Menu"))
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                ui.vertical_centered_justified(|ui| {
                    if ui.button(tr("Reprendre")).clicked() {
                        self.menu_open = false;
                        self.captured = true;
                    }
                    if ui.button(tr("Sauvegarder (F5)")).clicked() {
                        self.start_save();
                    }
                    if ui.button(tr("Retour à l'accueil")).clicked() {
                        action = SimAction::BackHome;
                    }
                    if ui.button(tr("Quitter")).clicked() {
                        action = SimAction::Quit;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(tr("Langue"));
                    crate::i18n::picker(ui);
                });
                ui.separator();
                ui.label(RichText::new(tr("Commandes")).strong());
                egui::Grid::new("keys")
                    .num_columns(2)
                    .spacing([16.0, 2.0])
                    .show(ui, |ui| {
                        for (k, v) in [
                            (
                                tr("Z / S"),
                                tr("avancer / reculer (en suivi : distance à la cible)"),
                            ),
                            (tr("Q / D"), tr("gauche / droite (sans effet en suivi)")),
                            (tr("A / E"), tr("basculer la vue autour de l'axe de visée")),
                            ("H", tr("redresser l'horizon sur le plan du disque")),
                            (tr("Maj / Ctrl"), tr("accélérer / ralentir")),
                            (
                                "+ / −",
                                tr("vitesse (proportionnelle à la distance à la sélection)"),
                            ),
                            (
                                tr("Souris"),
                                tr("orientation (en suivi : tourner autour de la cible)"),
                            ),
                            (tr("Molette"), tr("zoom optique")),
                            (tr("Clic gauche"), tr("suivre le corps visé")),
                            ("F", tr("suivre la sélection / cesser le suivi")),
                            ("G", tr("grille du plan du disque")),
                            (tr("Origine"), tr("corps le plus massif")),
                            (tr("PgDn / PgUp"), tr("corps suivant / précédent par masse")),
                            ("B", tr("barycentre : sélection, vue de face au disque")),
                            ("C", tr("viser le centre de gravité")),
                            ("V", tr("vue d'ensemble (90 % de la masse)")),
                            ("T", tr("tracer les 10 plus massifs visibles / effacer")),
                            ("R", tr("traces : référentiel corps sélectionné / barycentre / amas local")),
                            (tr("Espace / P"), tr("pause / pas à pas")),
                            (tr("Tab"), tr("libérer / capturer la souris")),
                            (tr("Clic droit"), tr("reprendre la visée (souris libre)")),
                            ("F5", "sauvegarder"),
                            ("F9", tr("démarrer / arrêter l'enregistrement")),
                            (tr("Espace"), tr("relecture : lecture / pause")),
                            ("← / →", tr("relecture : instantané précédent / suivant")),
                            ("↑ / ↓", tr("relecture : vitesse")),
                            ("⌫", tr("relecture : sens de lecture")),
                            (tr("Échap"), tr("ce menu")),
                        ] {
                            ui.label(RichText::new(k).monospace());
                            ui.label(v);
                            ui.end_row();
                        }
                    });
            });
        action
    }

    /// Journal de mesure pour les essais (`STELLAR_STATS=1`) : une ligne par
    /// seconde sur la sortie d'erreur.
    fn log_stats(&mut self, now: Instant) {
        let Some(last) = self.last_stats else { return };
        if now - last < std::time::Duration::from_secs(1) {
            return;
        }
        self.last_stats = Some(now);
        let snap = &self.snapshot;
        eprintln!(
            "stats : pas {} · {:.1} pas/s · {:.1} ms par pas · {:.0} images/s · {} corps",
            snap.step, snap.steps_per_second, snap.last_step_ms, self.fps, snap.body_count
        );
    }

    /// Image courante. Renvoie l'action demandée par l'utilisateur.
    pub fn show(&mut self, ctx: &egui::Context) -> SimAction {
        let now = Instant::now();
        let dt = self
            .last_frame
            .map_or(1.0 / 60.0, |t| (now - t).as_secs_f64())
            .min(0.25);
        self.last_frame = Some(now);
        if dt > 0.0 {
            self.fps = if self.fps == 0.0 {
                1.0 / dt
            } else {
                0.95 * self.fps + 0.05 / dt
            };
        }

        self.source.tick(dt);
        self.poll_snapshot();
        self.log_stats(now);
        self.refresh_disk_normal(false);
        self.sync_watch();
        self.poll_save();
        let mut action = self.handle_shortcuts(ctx);

        self.hud(ctx);
        self.sync_trace_reference();
        if let a @ (SimAction::BackHome | SimAction::Quit) = self.menu(ctx) {
            action = a;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(2, 2, 6)))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                self.handle_navigation(ctx, dt, rect);
                self.paint_scene(ui, rect);
            });

        self.apply_capture(ctx);
        ctx.request_repaint();
        action
    }

    /// Termine proprement la simulation.
    pub fn shutdown(self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        self.source.shutdown();
    }
}

/// Référentiel d'affichage des traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceFrame {
    /// Le corps sélectionné.
    Body,
    /// Le barycentre de la simulation.
    Barycenter,
    /// Le barycentre de l'amas lié du corps sélectionné.
    Cluster,
}

impl TraceFrame {
    fn label(self) -> &'static str {
        match self {
            TraceFrame::Body => tr("Corps sélectionné"),
            TraceFrame::Barycenter => tr("Barycentre"),
            TraceFrame::Cluster => tr("Amas local"),
        }
    }
}

