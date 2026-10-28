//! Écran d'accueil : création d'une simulation ou chargement d'une sauvegarde.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use stellar_application::{Continuation, RecordingSummary, SaveData, SaveSummary, SimulatorService};
use stellar_domain::{
    DensityProfile, Distribution, InitialConditions, PhysicsParams, SimulationConfig, VelocityMode,
};

use crate::format;

/// Options d'enregistrement choisies à la création.
#[derive(Clone, Copy)]
pub struct RecordOptions {
    pub interval: u32,
    pub dust_budget: Option<u64>,
}

pub enum HomeAction {
    None,
    Start(SimulationConfig, Option<RecordOptions>),
    Load(PathBuf),
    /// Charge une sauvegarde déjà lue et poursuit l'enregistrement indiqué.
    LoadContinuing(Box<SaveData>, PathBuf),
    Replay(PathBuf),
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VelKind {
    Rest,
    Random,
    CircularFixed,
    CircularOrbital,
    Radial,
}

impl VelKind {
    const ALL: [VelKind; 5] = [
        VelKind::Rest,
        VelKind::Random,
        VelKind::CircularFixed,
        VelKind::CircularOrbital,
        VelKind::Radial,
    ];

    fn label(self) -> &'static str {
        match self {
            VelKind::Rest => "Repos",
            VelKind::Random => "Aléatoire (isotrope)",
            VelKind::CircularFixed => "Circulaire — vitesse imposée",
            VelKind::CircularOrbital => "Circulaire — vitesse orbitale",
            VelKind::Radial => "Radiale, vers le centre",
        }
    }
}

pub struct HomeScreen {
    count: usize,
    radius: f64,
    seed: u64,
    disk: bool,
    disk_thickness: f64,
    disk_exponential: bool,
    disk_scale: f64,
    vel_kind: VelKind,
    vel_min: f64,
    vel_max: f64,
    vel_spread: f64,
    g: f64,
    auto_physics: bool,
    physics: PhysicsParams,
    engine: String,
    saves: Vec<SaveSummary>,
    saves_error: Option<String>,
    recordings: Vec<RecordingSummary>,
    record: bool,
    record_interval: u32,
    record_budget_enabled: bool,
    record_budget: u64,
    pub message: Option<String>,
    continue_dialog: Option<ContinueDialog>,
}

/// Choix de l'enregistrement à poursuivre depuis une sauvegarde.
struct ContinueDialog {
    save_name: String,
    data: Box<SaveData>,
    options: Vec<Continuation>,
}

impl HomeScreen {
    pub fn new(service: &SimulatorService) -> Self {
        let ic = InitialConditions::default();
        let mut s = Self {
            count: ic.count,
            radius: ic.radius,
            seed: ic.seed,
            disk: false,
            disk_thickness: 0.05,
            disk_exponential: false,
            disk_scale: 0.25,
            vel_kind: VelKind::Rest,
            vel_min: 0.0,
            vel_max: 10.0,
            vel_spread: 0.05,
            g: 1.0,
            auto_physics: true,
            physics: PhysicsParams::suggested(&ic, 1.0),
            engine: service.default_engine().unwrap_or_default().to_owned(),
            saves: Vec::new(),
            saves_error: None,
            recordings: Vec::new(),
            record: false,
            record_interval: 10,
            record_budget_enabled: true,
            record_budget: 1_000_000,
            message: None,
            continue_dialog: None,
        };
        s.refresh(service);
        s
    }

    pub fn refresh(&mut self, service: &SimulatorService) {
        match service.list_saves() {
            Ok(list) => {
                self.saves = list;
                self.saves_error = None;
            }
            Err(e) => self.saves_error = Some(e.to_string()),
        }
        match service.list_recordings() {
            Ok(list) => self.recordings = list,
            Err(e) => self.saves_error = Some(e.to_string()),
        }
    }

    fn initial_conditions(&self) -> InitialConditions {
        let distribution = if self.disk {
            Distribution::Disk {
                thickness: self.disk_thickness,
                profile: if self.disk_exponential {
                    DensityProfile::Exponential {
                        scale: self.disk_scale,
                    }
                } else {
                    DensityProfile::Uniform
                },
            }
        } else {
            Distribution::Sphere
        };
        let (min, max) = (self.vel_min, self.vel_max);
        let velocity = match self.vel_kind {
            VelKind::Rest => VelocityMode::Rest,
            VelKind::Random => VelocityMode::Random { min, max },
            VelKind::CircularFixed => VelocityMode::CircularFixed { min, max },
            VelKind::CircularOrbital => VelocityMode::CircularOrbital {
                spread: self.vel_spread,
            },
            VelKind::Radial => VelocityMode::Radial { min, max },
        };
        InitialConditions {
            count: self.count,
            radius: self.radius,
            seed: self.seed,
            distribution,
            velocity,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, service: &SimulatorService) -> HomeAction {
        let mut action = HomeAction::None;
        let ic = self.initial_conditions();
        self.apply_auto(&ic);

        egui::TopBottomPanel::bottom("home-footer").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(
                        "Stellar — simulateur gravitationnel N-corps (calcul CPU, rendu GPU)",
                    )
                    .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Quitter").clicked() {
                        action = HomeAction::Quit;
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.heading(RichText::new("STELLAR").size(34.0).strong());
                ui.label(
                    RichText::new("Effondrement gravitationnel et fusion de particules").italics(),
                );
                ui.add_space(8.0);
            });
            if let Some(msg) = &self.message {
                ui.colored_label(egui::Color32::from_rgb(255, 170, 90), msg);
            }
            ui.separator();

            ui.columns(2, |cols| {
                egui::ScrollArea::vertical()
                    .id_salt("new-sim")
                    .show(&mut cols[0], |ui| {
                        if let Some(a) = self.new_simulation_form(ui, service, &ic) {
                            action = a;
                        }
                    });
                egui::ScrollArea::vertical()
                    .id_salt("saves")
                    .show(&mut cols[1], |ui| {
                        if let Some(a) = self.saves_panel(ui, service) {
                            action = a;
                        }
                    });
            });
        });
        if let Some(a) = self.continue_window(ctx) {
            action = a;
        }
        action
    }

    /// Fenêtre de choix de l'enregistrement à poursuivre.
    fn continue_window(&mut self, ctx: &egui::Context) -> Option<HomeAction> {
        let dialog = self.continue_dialog.as_ref()?;
        let mut chosen = None;
        let mut close = false;
        egui::Window::new("Poursuivre un enregistrement")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(560.0);
                ui.label(format!(
                    "Sauvegarde « {} » (pas {}). L'enregistrement choisi reprend à ce pas ; ses instantanés postérieurs sont supprimés.",
                    dialog.save_name,
                    format::count(dialog.data.state.step)
                ));
                ui.add_space(6.0);
                if dialog.options.is_empty() {
                    ui.label(
                        RichText::new("Aucun enregistrement compatible : il faut les mêmes conditions initiales et un instantané antérieur à la sauvegarde.")
                            .weak(),
                    );
                }
                for c in &dialog.options {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                let mut title = RichText::new(&c.summary.name).strong();
                                if !c.summary.complete {
                                    title = title.color(egui::Color32::from_rgb(255, 170, 90));
                                }
                                ui.label(title);
                                let date = chrono::DateTime::from_timestamp(c.summary.created_at_unix, 0)
                                    .map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y %H:%M").to_string())
                                    .unwrap_or_default();
                                ui.label(
                                    RichText::new(format!(
                                        "{date} · {} instantanés{} · reprise après le pas {}",
                                        format::count(c.summary.frame_count as u64),
                                        if c.summary.complete { "" } else { " · non clos" },
                                        format::count(c.resume_step),
                                    ))
                                    .weak(),
                                );
                                if c.dropped_frames > 0 {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} instantanés postérieurs seront supprimés",
                                            format::count(c.dropped_frames as u64)
                                        ))
                                        .small()
                                        .color(egui::Color32::from_rgb(255, 120, 120)),
                                    );
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Poursuivre").clicked() {
                                    chosen = Some(c.summary.path.clone());
                                }
                            });
                        });
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Annuler").clicked() {
                        close = true;
                    }
                });
            });
        if let Some(path) = chosen {
            let d = self.continue_dialog.take()?;
            return Some(HomeAction::LoadContinuing(d.data, path));
        }
        if close {
            self.continue_dialog = None;
        }
        None
    }

    fn open_continue_dialog(&mut self, service: &SimulatorService, save: &SaveSummary) {
        match service.read_save(&save.path) {
            Ok(data) => {
                let options = service.continuable_recordings(&data);
                self.continue_dialog = Some(ContinueDialog {
                    save_name: save.name.clone(),
                    data: Box::new(data),
                    options,
                });
            }
            Err(e) => self.message = Some(format!("Lecture de la sauvegarde impossible : {e}")),
        }
    }

    fn new_simulation_form(
        &mut self,
        ui: &mut egui::Ui,
        service: &SimulatorService,
        ic: &InitialConditions,
    ) -> Option<HomeAction> {
        let mut action = None;
        ui.heading("Nouvelle simulation");
        ui.add_space(4.0);

        egui::Grid::new("ic-grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Nombre de particules");
                ui.add(
                    egui::DragValue::new(&mut self.count)
                        .range(2..=50_000_000)
                        .speed(1000.0),
                );
                ui.end_row();

                ui.label("Rayon du nuage");
                ui.add(
                    egui::DragValue::new(&mut self.radius)
                        .range(1e-3..=1e9)
                        .speed(1.0),
                );
                ui.end_row();

                ui.label("Graine aléatoire");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.seed).speed(1.0));
                    if ui.button("🎲").on_hover_text("Nouvelle graine").clicked() {
                        self.seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64 % 1_000_000_000)
                            .unwrap_or(1);
                    }
                });
                ui.end_row();

                ui.label("Répartition");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.disk, false, "Sphère");
                    ui.radio_value(&mut self.disk, true, "Disque");
                });
                ui.end_row();

                if self.disk {
                    ui.label("   Épaisseur (× rayon)");
                    ui.add(
                        egui::DragValue::new(&mut self.disk_thickness)
                            .range(0.0..=1.0)
                            .speed(0.002),
                    );
                    ui.end_row();
                    ui.label("   Profil de densité");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.disk_exponential, false, "Uniforme");
                        ui.radio_value(&mut self.disk_exponential, true, "Exponentiel");
                    });
                    ui.end_row();
                    if self.disk_exponential {
                        ui.label("   Longueur d'échelle (× rayon)");
                        ui.add(
                            egui::DragValue::new(&mut self.disk_scale)
                                .range(0.01..=2.0)
                                .speed(0.005),
                        );
                        ui.end_row();
                    }
                }

                ui.label("Vitesse initiale");
                egui::ComboBox::from_id_salt("vel-kind")
                    .selected_text(self.vel_kind.label())
                    .show_ui(ui, |ui| {
                        for k in VelKind::ALL {
                            ui.selectable_value(&mut self.vel_kind, k, k.label());
                        }
                    });
                ui.end_row();

                match self.vel_kind {
                    VelKind::Rest => {}
                    VelKind::CircularOrbital => {
                        ui.label("   Dispersion (±)");
                        ui.horizontal(|ui| {
                            let mut pct = self.vel_spread * 100.0;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut pct)
                                        .range(0.0..=100.0)
                                        .suffix(" %"),
                                )
                                .changed()
                            {
                                self.vel_spread = pct / 100.0;
                            }
                        });
                        ui.end_row();
                    }
                    _ => {
                        ui.label("   Norme : plage");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.vel_min)
                                    .range(0.0..=1e9)
                                    .speed(0.1)
                                    .prefix("min "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.vel_max)
                                    .range(0.0..=1e9)
                                    .speed(0.1)
                                    .prefix("max "),
                            );
                        });
                        ui.end_row();
                    }
                }
            });
        ui.label(
            RichText::new(format!(
                "Vitesse caractéristique √(GM/R) ≈ {} · temps de chute libre ≈ {}",
                format::real(ic.characteristic_speed(self.g)),
                format::real(ic.free_fall_time(self.g)),
            ))
            .weak(),
        );

        ui.add_space(6.0);
        egui::CollapsingHeader::new("Paramètres avancés")
            .default_open(false)
            .show(ui, |ui| self.advanced(ui, service, ic));

        ui.add_space(6.0);
        ui.checkbox(
            &mut self.record,
            "Enregistrer dès le départ (pour relecture fluide)",
        );
        if self.record {
            ui.horizontal(|ui| {
                ui.label("   Un instantané tous les");
                ui.add(egui::DragValue::new(&mut self.record_interval).range(1..=100_000));
                ui.label("pas");
            });
            ui.horizontal(|ui| {
                ui.label("  ");
                ui.checkbox(&mut self.record_budget_enabled, "Poussière limitée à");
                ui.add_enabled(
                    self.record_budget_enabled,
                    egui::DragValue::new(&mut self.record_budget)
                        .range(1_000..=100_000_000)
                        .speed(10_000.0),
                );
                ui.label("particules");
            });
            let kept = if self.record_budget_enabled {
                self.count.min(self.record_budget as usize)
            } else {
                self.count
            };
            ui.label(
                RichText::new(format!(
                    "≈ {:.0} Mo par instantané au départ (diminue avec les fusions)",
                    kept as f64 * 12.0 / 1e6
                ))
                .weak(),
            );
        }

        ui.add_space(10.0);
        // Mesuré : ≈ 4,2 Go pour 10 millions de corps (moteur, instantanés, rendu).
        let size = |mb: f64| {
            if mb >= 1000.0 {
                format!("{:.1} Go", mb / 1000.0).replace('.', ",")
            } else {
                format!("{mb:.0} Mo")
            }
        };
        let mut text = format!(
            "Mémoire estimée : ≈ {}",
            size(self.count as f64 * 420.0 / 1e6)
        );
        if self.engine.starts_with("gpu") {
            // Moteur GPU : ≈ 330 octets par corps en mémoire vidéo.
            text.push_str(&format!(
                " · mémoire vidéo ≈ {}",
                size(self.count as f64 * 330.0 / 1e6)
            ));
        }
        ui.label(RichText::new(text).weak());
        if ui
            .add_sized(
                [260.0, 36.0],
                egui::Button::new(RichText::new("▶  Lancer la simulation").size(17.0)),
            )
            .clicked()
        {
            let record = self.record.then_some(RecordOptions {
                interval: self.record_interval.max(1),
                dust_budget: self.record_budget_enabled.then_some(self.record_budget),
            });
            action = Some(HomeAction::Start(
                SimulationConfig {
                    initial: *ic,
                    physics: self.physics,
                    engine: self.engine.clone(),
                },
                record,
            ));
        }
        action
    }

    fn advanced(&mut self, ui: &mut egui::Ui, service: &SimulatorService, ic: &InitialConditions) {
        ui.horizontal(|ui| {
            ui.label("Constante G");
            ui.add(
                egui::DragValue::new(&mut self.g)
                    .range(1e-12..=1e12)
                    .speed(0.01),
            );
        });
        ui.checkbox(
            &mut self.auto_physics,
            "Déduire automatiquement dt, ε, r₀ et la liaison des amas des conditions initiales",
        );
        if !self.auto_physics && ui.button("Recalculer les valeurs suggérées").clicked() {
            self.physics = PhysicsParams::suggested(ic, self.g);
        }
        self.physics.g = self.g;

        ui.add_enabled_ui(!self.auto_physics, |ui| {
            egui::Grid::new("phys-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    let p = &mut self.physics;
                    ui.label("Pas de temps dt");
                    let sp = p.dt * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.dt)
                            .range(1e-12..=1e6)
                            .speed(sp)
                            .max_decimals(8),
                    );
                    ui.end_row();
                    ui.label("Adoucissement ε");
                    let sp = p.softening.max(1e-6) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.softening)
                            .range(0.0..=1e9)
                            .speed(sp)
                            .max_decimals(6),
                    );
                    ui.end_row();
                    ui.label("Rayon r₀ (masse 1)");
                    let sp = p.base_radius.max(1e-6) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.base_radius)
                            .range(0.0..=1e9)
                            .speed(sp)
                            .max_decimals(6),
                    );
                    ui.end_row();
                    ui.label("Amas éjectés : liaison")
                        .on_hover_text(
                            "Deux corps lointains plus proches que cette distance                              appartiennent au même groupe (par défaut : distance                              moyenne entre voisins au départ).",
                        );
                    let sp = p.cluster_link_length.max(1e-6) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.cluster_link_length)
                            .range(0.0..=1e9)
                            .speed(sp)
                            .max_decimals(6),
                    );
                    ui.end_row();
                });
        });
        egui::Grid::new("phys-grid-2")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                let p = &mut self.physics;
                ui.label("Précision Barnes-Hut θ");
                ui.add(egui::Slider::new(&mut p.theta, 0.1..=1.0));
                ui.end_row();
                ui.label("Nettoyage : période (pas)");
                ui.add(egui::DragValue::new(&mut p.cleanup_period).range(0..=1_000_000));
                ui.end_row();
                ui.label("Nettoyage : distance (× rayon)");
                ui.add(
                    egui::DragValue::new(&mut p.cleanup_distance_factor)
                        .range(1.0..=1e6)
                        .speed(0.1),
                );
                ui.end_row();
                ui.label("Amas éjectés : corps au moins").on_hover_text(
                    "Un amas éjecté lié d'au moins ce nombre de corps est conservé                      par le nettoyage (0 : critère ignoré).",
                );
                ui.add(egui::DragValue::new(&mut p.cluster_min_bodies).range(0..=10_000_000));
                ui.end_row();
                ui.label("… ou masse au moins").on_hover_text(
                    "… ou d'au moins cette part de la masse initiale, même s'il                      ne compte qu'un corps (0 : critère ignoré).",
                );
                let mut percent = p.cluster_min_mass * 100.0;
                if ui
                    .add(
                        egui::DragValue::new(&mut percent)
                            .range(0.0..=100.0)
                            .speed(0.01)
                            .max_decimals(4)
                            .suffix(" %"),
                    )
                    .changed()
                {
                    p.cluster_min_mass = percent / 100.0;
                }
                ui.end_row();
                ui.label("Moteur de calcul");
                let engines = service.engines();
                let current = engines
                    .iter()
                    .find(|e| e.id == self.engine)
                    .map(|e| e.label)
                    .unwrap_or("?");
                egui::ComboBox::from_id_salt("engine")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for e in &engines {
                            ui.selectable_value(&mut self.engine, e.id.to_owned(), e.label)
                                .on_hover_text(e.description);
                        }
                    });
                ui.end_row();
            });
        if let Some(note) = service.engine_note() {
            ui.label(RichText::new(note).small().weak());
        }
        self.physics.reference_radius = ic.radius;
    }

    /// En mode automatique, dt, ε, r₀ et la liaison des amas suivent les
    /// conditions initiales ; θ, le nettoyage et les seuils des amas restent
    /// ceux saisis.
    fn apply_auto(&mut self, ic: &InitialConditions) {
        if self.auto_physics {
            let keep = self.physics;
            self.physics = PhysicsParams::suggested(ic, self.g);
            self.physics.theta = keep.theta;
            self.physics.cleanup_period = keep.cleanup_period;
            self.physics.cleanup_distance_factor = keep.cleanup_distance_factor;
            self.physics.cluster_min_bodies = keep.cluster_min_bodies;
            self.physics.cluster_min_mass = keep.cluster_min_mass;
        }
        self.physics.g = self.g;
        self.physics.reference_radius = ic.radius;
    }

    fn saves_panel(&mut self, ui: &mut egui::Ui, service: &SimulatorService) -> Option<HomeAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.heading("Simulations sauvegardées");
            if ui.button("⟳").on_hover_text("Actualiser").clicked() {
                self.refresh(service);
            }
            if ui.button("Parcourir…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Sauvegarde Stellar", &["stellar"])
                    .pick_file()
                {
                    action = Some(HomeAction::Load(path));
                }
            }
        });
        ui.add_space(4.0);
        if let Some(e) = &self.saves_error {
            ui.colored_label(egui::Color32::LIGHT_RED, e);
        }
        if self.saves.is_empty() {
            ui.label(RichText::new("Aucune sauvegarde dans le dossier « saves ». Appuyez sur F5 pendant une simulation.").weak());
        }
        let mut to_continue = None;
        for s in &self.saves {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&s.name).strong());
                        let date = chrono::DateTime::from_timestamp(s.saved_at_unix, 0)
                            .map(|d| {
                                d.with_timezone(&chrono::Local)
                                    .format("%d/%m/%Y %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(format!(
                                "{date} · {} corps · pas {} · t = {}",
                                format::count(s.body_count as u64),
                                format::count(s.step),
                                format::real(s.time)
                            ))
                            .weak(),
                        );
                        if let Some(rec) = &s.recording {
                            ui.label(
                                RichText::new(format!("Reprend l'enregistrement « {rec} »"))
                                    .small()
                                    .color(egui::Color32::from_rgb(255, 120, 120)),
                            );
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Charger").clicked() {
                            action = Some(HomeAction::Load(s.path.clone()));
                        }
                        if ui
                            .button("Poursuivre un enregistrement…")
                            .on_hover_text("Charger cette sauvegarde et prolonger un enregistrement existant à partir de son pas")
                            .clicked()
                        {
                            to_continue = Some(s.clone());
                        }
                    });
                });
            });
        }
        if let Some(s) = to_continue {
            self.open_continue_dialog(service, &s);
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.heading("Enregistrements");
            if ui.button("Parcourir…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Enregistrement Stellar", &["stellarec"])
                    .pick_file()
                {
                    action = Some(HomeAction::Replay(path));
                }
            }
        });
        if self.recordings.is_empty() {
            ui.label(RichText::new("Aucun enregistrement dans le dossier « recordings ». Appuyez sur F9 pendant une simulation.").weak());
        }
        for r in &self.recordings {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let mut title = RichText::new(&r.name).strong();
                        if !r.complete {
                            title = title.color(egui::Color32::from_rgb(255, 170, 90));
                        }
                        ui.label(title);
                        let date = chrono::DateTime::from_timestamp(r.created_at_unix, 0)
                            .map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y %H:%M").to_string())
                            .unwrap_or_default();
                        let size = if r.bytes >= 1_000_000_000 {
                            format!("{:.2} Go", r.bytes as f64 / 1e9)
                        } else {
                            format!("{:.0} Mo", r.bytes as f64 / 1e6)
                        }
                        .replace('.', ",");
                        ui.label(
                            RichText::new(format!(
                                "{date} · {} instantanés · {} corps au départ · durée {} · {size}{}",
                                format::count(r.frame_count as u64),
                                format::count(r.first_count as u64),
                                format::real(r.duration),
                                if r.complete { "" } else { " · non clos (récupéré)" }
                            ))
                            .weak(),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Rejouer").clicked() {
                            action = Some(HomeAction::Replay(r.path.clone()));
                        }
                        // Sauvegarde de reprise la plus récente liée à cet enregistrement.
                        let file = r.path.file_name().map(|n| n.to_string_lossy().into_owned());
                        if let Some(save) = self.saves.iter().find(|s| s.recording.is_some() && s.recording == file) {
                            if ui
                                .button("Reprendre")
                                .on_hover_text(format!(
                                    "Recharge la sauvegarde « {} » (pas {}) et poursuit l'enregistrement",
                                    save.name,
                                    format::count(save.step)
                                ))
                                .clicked()
                            {
                                action = Some(HomeAction::Load(save.path.clone()));
                            }
                        }
                    });
                });
            });
        }
        action
    }
}
