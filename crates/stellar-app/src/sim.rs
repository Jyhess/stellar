//! Écran de simulation : navigation, sélection, HUD, sauvegarde.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Pos2, RichText, Stroke};
use eframe::egui_wgpu;
use glam::DVec3;
use stellar_application::{Phase, SimulationHandle, SimulatorService, Snapshot, ViewState};
use stellar_domain::physics::body_radius;
use stellar_domain::{ranking, BodyId};

use crate::camera::{Camera, Mode, MAX_FOV, MIN_FOV};
use crate::format;
use crate::picking::pick;
use crate::render::{mass_color, Instance, InstanceBatch, PointsCallback, Uniforms};

pub enum SimAction {
    None,
    BackHome,
    Quit,
}

const MOUSE_SENSITIVITY: f64 = 0.0022;
const SCROLL_SENSITIVITY: f64 = 0.0025;
const PICK_TOLERANCE_PX: f64 = 10.0;
const TOAST_DURATION: Duration = Duration::from_secs(4);

/// Données dérivées d'un instantané, recalculées à chaque publication.
struct SnapshotCache {
    barycenter: DVec3,
    selected_index: Option<usize>,
    selected_rank: Option<usize>,
}

pub struct SimScreen {
    handle: SimulationHandle,
    service: SimulatorService,
    camera: Camera,
    captured: bool,
    capture_applied: Option<bool>,
    menu_open: bool,
    selected: Option<BodyId>,
    paused: bool,

    snapshot: Arc<Snapshot>,
    seen_sequence: u64,
    cache: SnapshotCache,

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
    toast: Option<(String, Instant)>,
    save_job: Option<Receiver<Result<PathBuf, String>>>,
}

impl SimScreen {
    pub fn new(handle: SimulationHandle, service: SimulatorService, view: Option<ViewState>) -> Self {
        let r = handle.config().physics.reference_radius.max(1e-9);
        let speed = 0.3 * r;
        let camera = match &view {
            Some(v) => Camera::from_view(v, speed),
            None => Camera::looking_at(DVec3::new(0.0, -2.6 * r, 0.9 * r), DVec3::ZERO, speed),
        };
        let selected = camera.followed();
        let snapshot = handle.latest();
        let paused = !snapshot.running;
        Self {
            handle,
            service,
            camera,
            captured: true,
            capture_applied: None,
            menu_open: false,
            selected,
            paused,
            seen_sequence: u64::MAX,
            snapshot,
            cache: SnapshotCache { barycenter: DVec3::ZERO, selected_index: None, selected_rank: None },
            anchor: DVec3::ZERO,
            batch: None,
            generation: 0,
            need_upload: true,
            brightness: 1.0,
            dust_floor: 0.2,
            min_pixels: 1.6,
            limit_rate: false,
            max_rate: 30.0,
            fps: 0.0,
            last_frame: None,
            toast: None,
            save_job: None,
        }
    }

    fn reference_radius(&self) -> f64 {
        self.handle.config().physics.reference_radius.max(1e-9)
    }

    fn base_radius(&self) -> f64 {
        self.handle.config().physics.base_radius
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    // ------------------------------------------------------------------
    // Instantanés
    // ------------------------------------------------------------------

    fn poll_snapshot(&mut self) {
        let seq = self.handle.sequence();
        if seq == self.seen_sequence {
            return;
        }
        self.seen_sequence = seq;
        self.snapshot = self.handle.latest();
        let bodies = &self.snapshot.state.bodies;
        self.cache.barycenter = bodies.center_of_mass().unwrap_or(DVec3::ZERO);

        // Suivi de la sélection à travers fusions et éjections.
        if let Some(id) = self.selected {
            if self.snapshot.phase == Phase::Ready {
                match self.handle.resolve(id) {
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
                        self.notify("Le corps suivi a été éjecté et éliminé.");
                    }
                }
            }
        }
        self.refresh_selection_cache();
        self.need_upload = true;
    }

    fn refresh_selection_cache(&mut self) {
        let bodies = &self.snapshot.state.bodies;
        self.cache.selected_index = self.selected.and_then(|id| bodies.index_of(id));
        self.cache.selected_rank = self
            .cache
            .selected_index
            .map(|i| ranking::rank_of(&bodies.ids, &bodies.masses, (bodies.masses[i], bodies.ids[i])));
        if self.selected.is_some() && self.cache.selected_index.is_none() && self.snapshot.phase == Phase::Ready {
            self.selected = None;
            self.camera.mode = Mode::Free;
        }
    }

    fn select_index(&mut self, i: usize) {
        let bodies = &self.snapshot.state.bodies;
        let p = bodies.positions[i];
        let r = body_radius(bodies.masses[i], self.base_radius()).max(1e-9);
        let current = (p - self.camera.position).length();
        let preferred = (60.0 * r).max(0.03 * self.reference_radius());
        let distance = current.min(preferred).max(3.0 * r);
        self.selected = Some(bodies.ids[i]);
        self.camera.look_towards(p - self.camera.position);
        self.camera.mode = Mode::Orbit { target: bodies.ids[i], distance };
        self.refresh_selection_cache();
        self.camera.place_in_orbit(p);
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
        if let Some(i) = ranking::neighbour(&b.ids, &b.masses, (b.masses[cur], b.ids[cur]), lighter) {
            self.select_index(i);
        } else {
            self.notify(if lighter { "C'est déjà le corps le plus léger." } else { "C'est déjà le corps le plus massif." });
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
            if !self.paused {
                self.toggle_pause();
            }
            self.handle.step_once();
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
        self.paused = !self.paused;
        if self.paused {
            self.handle.pause();
        } else {
            self.handle.resume();
        }
    }

    fn handle_navigation(&mut self, ctx: &egui::Context, dt: f64, viewport: egui::Rect) {
        if self.menu_open {
            return;
        }
        let (moves, mouse, scroll, shift, ctrl) = ctx.input(|i| {
            let k = |key: Key| if i.key_down(key) { 1.0 } else { 0.0 };
            let moves = DVec3::new(
                k(Key::D) - k(Key::Q),
                k(Key::E) - k(Key::A),
                k(Key::Z) - k(Key::S),
            );
            let mut mouse = egui::Vec2::ZERO;
            for e in &i.events {
                if let egui::Event::MouseMoved(d) = e {
                    mouse += *d;
                }
            }
            (moves, mouse, i.smooth_scroll_delta.y as f64, i.modifiers.shift, i.modifiers.ctrl)
        });

        // Orientation (souris capturée uniquement).
        if self.captured && mouse != egui::Vec2::ZERO {
            self.camera.rotate(-(mouse.x as f64) * MOUSE_SENSITIVITY, -(mouse.y as f64) * MOUSE_SENSITIVITY);
        }

        // Déplacement : toute touche ZQSD/AE rend la main en vol libre.
        if moves != DVec3::ZERO {
            if matches!(self.camera.mode, Mode::Orbit { .. }) {
                self.camera.mode = Mode::Free;
            }
            let boost = if shift { 5.0 } else if ctrl { 0.2 } else { 1.0 };
            self.camera.translate_local(moves.normalize() * self.camera.speed * boost * dt);
        }

        // Molette : zoom optique en vol libre, distance en orbite.
        let over_ui = ctx.is_pointer_over_area() && !self.captured;
        if scroll != 0.0 && !over_ui {
            let k = (-scroll * SCROLL_SENSITIVITY).exp();
            let r = self
                .cache
                .selected_index
                .map(|i| body_radius(self.snapshot.state.bodies.masses[i], self.base_radius()))
                .unwrap_or(0.0);
            match &mut self.camera.mode {
                Mode::Free => self.camera.fov_y = (self.camera.fov_y * k).clamp(MIN_FOV, MAX_FOV),
                Mode::Orbit { distance, .. } => {
                    *distance = (*distance * k).max(1.5 * r).max(1e-9);
                }
            }
        }

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
        if let (Mode::Orbit { .. }, Some(i)) = (self.camera.mode, self.cache.selected_index) {
            let p = self.snapshot.state.bodies.positions[i];
            self.camera.place_in_orbit(p);
        }
    }

    // ------------------------------------------------------------------
    // Sauvegarde
    // ------------------------------------------------------------------

    fn start_save(&mut self) {
        if self.save_job.is_some() {
            return;
        }
        if self.snapshot.phase != Phase::Ready {
            self.notify("La simulation n'est pas encore prête.");
            return;
        }
        let (tx, rx) = mpsc::channel();
        let service = self.service.clone();
        let config = self.handle.config().clone();
        let snapshot = Arc::clone(&self.snapshot);
        let view = self.camera.to_view();
        let name = format!("sim_{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
        std::thread::spawn(move || {
            let r = service.save(&name, &config, &snapshot, Some(view)).map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.save_job = Some(rx);
        self.notify("Sauvegarde en cours…");
    }

    fn poll_save(&mut self) {
        let Some(rx) = &self.save_job else { return };
        if let Ok(r) = rx.try_recv() {
            self.save_job = None;
            match r {
                Ok(p) => self.notify(format!("Sauvegardé : {}", p.display())),
                Err(e) => self.notify(format!("Échec de la sauvegarde : {e}")),
            }
        }
    }

    // ------------------------------------------------------------------
    // Rendu
    // ------------------------------------------------------------------

    fn rebase_threshold(&self) -> f64 {
        match self.camera.mode {
            Mode::Orbit { distance, .. } => (50.0 * distance).min(0.05 * self.reference_radius()),
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
        let instances: Vec<Instance> = bodies
            .positions
            .iter()
            .zip(&bodies.masses)
            .map(|(p, &m)| {
                let rel = (*p - anchor).as_vec3();
                Instance {
                    pos_radius: [rel.x, rel.y, rel.z, body_radius(m, r0) as f32],
                    color: mass_color(m, max_mass),
                }
            })
            .collect();
        self.generation += 1;
        self.batch = Some(InstanceBatch { generation: self.generation, instances: Arc::new(instances) });
    }

    fn paint_scene(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        self.prepare_batch();
        let ppp = ui.ctx().pixels_per_point();
        let aspect = rect.width() / rect.height().max(1.0);
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
        };
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            PointsCallback { uniforms, batch: self.batch.clone() },
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

        // Repère du corps sélectionné.
        if let Some(i) = self.cache.selected_index {
            let b = &self.snapshot.state.bodies;
            if let Some(ndc) = self.camera.project(b.positions[i], aspect) {
                let c = to_screen(ndc);
                let d = (b.positions[i] - self.camera.position).length().max(1e-12);
                let r_world = body_radius(b.masses[i], self.base_radius());
                let r_px = (r_world / (d * (0.5 * self.camera.fov_y).tan()) * 0.5 * rect.height() as f64) as f32;
                let ring = r_px.max(6.0) + 6.0;
                painter.circle_stroke(c, ring, Stroke::new(1.5_f32, Color32::from_rgb(120, 220, 255)));
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
                format!(
                    "Génération de {} particules et premier calcul des forces…",
                    format::count(self.handle.config().initial.count as u64)
                ),
                egui::FontId::proportional(22.0),
                Color32::from_gray(220),
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
            "Tab : libérer la souris · Échap : menu · Origine / PgDn / PgUp : corps par masse · Espace : pause · F5 : sauvegarder"
        } else {
            "Souris libre — cliquez un corps pour le suivre · Tab : reprendre la visée · Échap : menu"
        };
        painter.text(
            rect.center_bottom() - egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            hint,
            egui::FontId::proportional(13.0),
            Color32::from_gray(150),
        );
    }

    // ------------------------------------------------------------------
    // HUD
    // ------------------------------------------------------------------

    fn hud(&mut self, ctx: &egui::Context) {
        let snap = Arc::clone(&self.snapshot);
        egui::Window::new("Simulation")
            .anchor(egui::Align2::LEFT_TOP, [10.0, 10.0])
            .resizable(false)
            .default_width(260.0)
            .show(ctx, |ui| {
                egui::Grid::new("stats").num_columns(2).spacing([14.0, 3.0]).show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.label(k);
                        ui.label(RichText::new(v).monospace());
                        ui.end_row();
                    };
                    let state = if snap.phase == Phase::Initialising {
                        "initialisation"
                    } else if self.paused {
                        "en pause"
                    } else {
                        "en cours"
                    };
                    row(ui, "État", state.to_owned());
                    row(ui, "Corps", format::count(snap.state.bodies.len() as u64));
                    row(ui, "Pas", format::count(snap.state.step));
                    row(ui, "Temps simulé", format::real(snap.state.time));
                    row(ui, "Pas / s", format!("{:.2}", snap.steps_per_second).replace('.', ","));
                    row(ui, "Durée d'un pas", format!("{:.1} ms", snap.last_step_ms).replace('.', ","));
                    row(ui, "Images / s", format!("{:.0}", self.fps));
                    row(ui, "Énergie totale", format::real(snap.diagnostics.total_energy()));
                    if let Some(d) = snap.energy_drift() {
                        row(ui, "ΔE / E₀", format::percent(d));
                    }
                });
                ui.label(RichText::new("ΔE inclut les pertes par fusion et éjection.").small().weak());
                ui.separator();
                ui.horizontal(|ui| {
                    let label = if self.paused { "▶ Reprendre" } else { "⏸ Pause" };
                    if ui.button(label).clicked() {
                        self.toggle_pause();
                    }
                    if ui.button("Pas à pas").clicked() {
                        if !self.paused {
                            self.toggle_pause();
                        }
                        self.handle.step_once();
                    }
                });
                ui.horizontal(|ui| {
                    let mut changed = ui.checkbox(&mut self.limit_rate, "Limiter à").changed();
                    changed |= ui
                        .add_enabled(
                            self.limit_rate,
                            egui::DragValue::new(&mut self.max_rate).range(0.1..=10_000.0).speed(0.5).suffix(" pas/s"),
                        )
                        .changed();
                    if changed {
                        self.handle.set_max_rate(self.limit_rate.then_some(self.max_rate));
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Luminosité");
                    ui.add(egui::Slider::new(&mut self.brightness, 0.02..=3.0).logarithmic(true));
                });
                ui.horizontal(|ui| {
                    ui.label("Poussières");
                    ui.add(egui::Slider::new(&mut self.dust_floor, 0.0..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label("Vitesse caméra");
                    let sp = self.camera.speed * 0.02;
                    ui.add(
                        egui::DragValue::new(&mut self.camera.speed)
                            .speed(sp)
                            .range(1e-9..=1e12),
                    );
                    ui.label(RichText::new("(+/−, Maj, Ctrl)").weak());
                });
            });

        let mut clicked_id: Option<BodyId> = None;
        egui::Window::new("Corps")
            .anchor(egui::Align2::RIGHT_TOP, [-10.0, 10.0])
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("Corps sélectionné").strong());
                match self.cache.selected_index {
                    Some(i) => {
                        let b = &snap.state.bodies;
                        let m = b.masses[i];
                        egui::Grid::new("sel").num_columns(2).spacing([14.0, 3.0]).show(ui, |ui| {
                            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                ui.label(k);
                                ui.label(RichText::new(v).monospace());
                                ui.end_row();
                            };
                            row(ui, "Identifiant", b.ids[i].to_string());
                            if let Some(r) = self.cache.selected_rank {
                                row(ui, "Rang (masse)", format!("{} / {}", format::count(r as u64), format::count(b.len() as u64)));
                            }
                            row(ui, "Masse", format::real(m));
                            row(ui, "Rayon", format::real(body_radius(m, self.base_radius())));
                            row(ui, "Vitesse", format::real(b.velocities[i].length()));
                            row(ui, "Dist. barycentre", format::real((b.positions[i] - self.cache.barycenter).length()));
                        });
                        if ui.button("Cesser le suivi").clicked() {
                            self.selected = None;
                            self.camera.mode = Mode::Free;
                            self.cache.selected_index = None;
                        }
                    }
                    None => {
                        ui.label(RichText::new("Aucun — visez et cliquez, ou touche Origine.").weak());
                    }
                }
                ui.separator();
                ui.label(RichText::new("Les plus massifs").strong());
                egui::Grid::new("top").num_columns(3).spacing([10.0, 2.0]).show(ui, |ui| {
                    for (rank, (id, m)) in snap.top.iter().enumerate() {
                        ui.label(format!("{}.", rank + 1));
                        let sel = self.selected == Some(*id);
                        if ui.selectable_label(sel, format!("#{id}")).clicked() {
                            clicked_id = Some(*id);
                        }
                        ui.label(RichText::new(format::real(*m)).monospace());
                        ui.end_row();
                    }
                });
            });
        if let Some(id) = clicked_id {
            if let Some(i) = self.snapshot.state.bodies.index_of(id) {
                self.select_index(i);
            }
        }
    }

    fn menu(&mut self, ctx: &egui::Context) -> SimAction {
        let mut action = SimAction::None;
        if !self.menu_open {
            return action;
        }
        egui::Window::new("Menu")
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                ui.vertical_centered_justified(|ui| {
                    if ui.button("Reprendre").clicked() {
                        self.menu_open = false;
                        self.captured = true;
                    }
                    if ui.button("Sauvegarder (F5)").clicked() {
                        self.start_save();
                    }
                    if ui.button("Retour à l'accueil").clicked() {
                        action = SimAction::BackHome;
                    }
                    if ui.button("Quitter").clicked() {
                        action = SimAction::Quit;
                    }
                });
                ui.separator();
                ui.label(RichText::new("Commandes").strong());
                egui::Grid::new("keys").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
                    for (k, v) in [
                        ("Z / S", "avancer / reculer"),
                        ("Q / D", "gauche / droite"),
                        ("E / A", "monter / descendre"),
                        ("Maj / Ctrl", "accélérer / ralentir le déplacement"),
                        ("+ / −", "vitesse de déplacement"),
                        ("Souris", "orientation (ou orbite autour du corps suivi)"),
                        ("Molette", "zoom (ou distance au corps suivi)"),
                        ("Clic gauche", "suivre le corps visé"),
                        ("Origine", "corps le plus massif"),
                        ("PgDn / PgUp", "corps suivant / précédent par masse"),
                        ("Espace / P", "pause / pas à pas"),
                        ("Tab", "libérer / capturer la souris"),
                        ("F5", "sauvegarder"),
                        ("Échap", "ce menu"),
                    ] {
                        ui.label(RichText::new(k).monospace());
                        ui.label(v);
                        ui.end_row();
                    }
                });
            });
        action
    }

    /// Image courante. Renvoie l'action demandée par l'utilisateur.
    pub fn show(&mut self, ctx: &egui::Context) -> SimAction {
        let now = Instant::now();
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| (now - t).as_secs_f64()).min(0.25);
        self.last_frame = Some(now);
        if dt > 0.0 {
            self.fps = if self.fps == 0.0 { 1.0 / dt } else { 0.95 * self.fps + 0.05 / dt };
        }

        self.poll_snapshot();
        self.poll_save();
        let mut action = self.handle_shortcuts(ctx);

        self.hud(ctx);
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
        self.handle.shutdown();
    }
}
