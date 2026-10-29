//! Écran d'accueil : création d'une simulation ou chargement d'une sauvegarde.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use stellar_application::{
    name_from_file, Continuation, RecordSettings, RecordingSummary, RunTarget, SaveData,
    SaveSummary, SimulatorService,
};
use stellar_domain::{
    DensityProfile, Distribution, InitialConditions, PhysicsParams, SimulationConfig, VelocityMode,
};

use crate::format;

use crate::i18n::tr;

/// Options d'enregistrement choisies à la création.
#[derive(Clone, Copy)]
pub struct RecordOptions {
    pub interval: u32,
    pub dust_budget: Option<u64>,
}

pub enum HomeAction {
    None,
    /// Configuration, enregistrement dès le départ, nom de la simulation.
    Start(SimulationConfig, Option<RecordOptions>, String),
    /// Simulation sans affichage : configuration, enregistrement, nom, cible.
    StartHeadless(SimulationConfig, RecordSettings, String, RunTarget),
    Load(PathBuf),
    /// Charge une sauvegarde déjà lue (nom de la sauvegarde) et poursuit
    /// l'enregistrement indiqué.
    LoadContinuing(Box<SaveData>, PathBuf, String),
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
            VelKind::Rest => tr("Repos"),
            VelKind::Random => tr("Aléatoire (isotrope)"),
            VelKind::CircularFixed => tr("Circulaire — vitesse imposée"),
            VelKind::CircularOrbital => tr("Circulaire — vitesse orbitale"),
            VelKind::Radial => tr("Radiale, vers le centre"),
        }
    }
}

pub struct HomeScreen {
    /// Nom de la nouvelle simulation ; tant qu'il n'est pas saisi, il suit
    /// les paramètres ([`default_name`]).
    name: String,
    name_edited: bool,
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
    /// Sans affichage (enregistrement automatique), et sa cible.
    headless: bool,
    target_kind: TargetKind,
    target_steps: u64,
    target_time: f64,
    target_minutes: f64,
    record_interval: u32,
    record_budget_enabled: bool,
    record_budget: u64,
    pub message: Option<String>,
    continue_dialog: Option<ContinueDialog>,
    file_dialog: Option<FileDialog>,
}

/// Fichier visé par une suppression ou un renommage.
#[derive(Clone)]
enum FileTarget {
    Save(SaveSummary),
    Recording(RecordingSummary),
}

impl FileTarget {
    fn name(&self) -> &str {
        match self {
            FileTarget::Save(s) => &s.name,
            FileTarget::Recording(r) => &r.name,
        }
    }
}

/// Confirmation d'une suppression, ou saisie d'un nouveau nom.
enum FileDialog {
    Delete {
        target: FileTarget,
        /// Fichiers liés (noms), proposés à la suppression.
        linked: Vec<String>,
        with_linked: bool,
    },
    Rename {
        target: FileTarget,
        name: String,
        /// Fichiers liés renommés avec lui.
        linked: Vec<String>,
    },
}

/// Condition d'arrêt d'une simulation sans affichage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Manual,
    Steps,
    SimTime,
    WallTime,
}

impl TargetKind {
    const ALL: [TargetKind; 4] = [
        TargetKind::Steps,
        TargetKind::SimTime,
        TargetKind::WallTime,
        TargetKind::Manual,
    ];

    fn label(self) -> &'static str {
        match self {
            TargetKind::Manual => tr("manuel (bouton d'arrêt)"),
            TargetKind::Steps => tr("après un nombre de pas"),
            TargetKind::SimTime => tr("après un temps simulé"),
            TargetKind::WallTime => tr("après une durée réelle"),
        }
    }
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
            name: String::new(),
            name_edited: false,
            count: ic.count,
            radius: ic.radius,
            seed: ic.seed,
            disk: false,
            disk_thickness: 0.05,
            disk_exponential: false,
            disk_scale: 0.25,
            vel_kind: VelKind::CircularOrbital,
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
            headless: false,
            target_kind: TargetKind::Steps,
            target_steps: 100_000,
            target_time: 10.0,
            target_minutes: 60.0,
            record_interval: 10,
            record_budget_enabled: true,
            record_budget: 1_000_000,
            message: None,
            continue_dialog: None,
            file_dialog: None,
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
                        tr("Stellar — simulateur gravitationnel N-corps (calcul CPU ou GPU, rendu GPU)"),
                    )
                    .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("Quitter")).clicked() {
                        action = HomeAction::Quit;
                    }
                    crate::i18n::picker(ui);
                    ui.label(tr("Langue"));
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.heading(RichText::new("STELLAR").size(34.0).strong());
                ui.label(
                    RichText::new(tr("Effondrement gravitationnel et fusion de particules")).italics(),
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
        self.file_window(ctx, service);
        action
    }

    /// Noms des fichiers liés à `target` : enregistrement d'une sauvegarde,
    /// sauvegardes d'un enregistrement.
    fn linked_names(service: &SimulatorService, target: &FileTarget) -> Vec<String> {
        match target {
            FileTarget::Save(s) => service
                .linked_recording(s)
                .map(|r| tr!("enregistrement « {} »", r.name))
                .into_iter()
                .collect(),
            FileTarget::Recording(r) => service
                .linked_saves(&r.path)
                .into_iter()
                .map(|s| tr!("sauvegarde « {} »", s.name))
                .collect(),
        }
    }

    fn ask_delete(&mut self, service: &SimulatorService, target: FileTarget) {
        let linked = Self::linked_names(service, &target);
        self.file_dialog = Some(FileDialog::Delete {
            target,
            linked,
            with_linked: false,
        });
    }

    fn ask_rename(&mut self, service: &SimulatorService, target: FileTarget) {
        let linked = Self::linked_names(service, &target);
        self.file_dialog = Some(FileDialog::Rename {
            name: target.name().to_owned(),
            target,
            linked,
        });
    }

    /// Fenêtre de confirmation (suppression) ou de saisie (renommage).
    fn file_window(&mut self, ctx: &egui::Context, service: &SimulatorService) {
        let Some(dialog) = &mut self.file_dialog else {
            return;
        };
        let (mut confirmed, mut cancelled) = (false, false);
        let title = match dialog {
            FileDialog::Delete { .. } => tr("Supprimer"),
            FileDialog::Rename { .. } => tr("Renommer"),
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                match dialog {
                    FileDialog::Delete {
                        target,
                        linked,
                        with_linked,
                    } => {
                        let what = match target {
                            FileTarget::Save(_) => tr("la sauvegarde"),
                            FileTarget::Recording(_) => tr("l'enregistrement"),
                        };
                        ui.label(tr!("Supprimer {} « {} » ?", what, target.name()));
                        ui.label(RichText::new(tr("Le fichier part dans la corbeille de Windows.")).weak());
                        if !linked.is_empty() {
                            ui.checkbox(
                                with_linked,
                                tr!("Supprimer aussi : {}", linked.join(", ")),
                            );
                        }
                    }
                    FileDialog::Rename { name, linked, .. } => {
                        let edit = ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            confirmed = true;
                        } else if ui.memory(|m| m.focused().is_none()) {
                            edit.request_focus();
                        }
                        if !linked.is_empty() {
                            ui.label(
                                RichText::new(tr!("Renommés avec lui : {}.", linked.join(", ")))
                                    .weak(),
                            );
                        }
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = match dialog {
                        FileDialog::Delete { .. } => tr("Supprimer"),
                        FileDialog::Rename { .. } => tr("Renommer"),
                    };
                    confirmed |= ui.button(ok).clicked();
                    cancelled |= ui.button(tr("Annuler")).clicked();
                });
            });
        if cancelled || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.file_dialog = None;
            return;
        }
        if !confirmed {
            return;
        }
        let result = match self.file_dialog.take() {
            Some(FileDialog::Delete {
                target: FileTarget::Save(s),
                with_linked,
                ..
            }) => service.delete_save(&s, with_linked),
            Some(FileDialog::Delete {
                target: FileTarget::Recording(r),
                with_linked,
                ..
            }) => service.delete_recording(&r, with_linked),
            Some(FileDialog::Rename {
                target: FileTarget::Save(s),
                name,
                ..
            }) => service.rename_save(&s, name.trim()),
            Some(FileDialog::Rename {
                target: FileTarget::Recording(r),
                name,
                ..
            }) => service.rename_recording(&r, name.trim()),
            None => Ok(()),
        };
        if let Err(e) = result {
            self.message = Some(tr!("Opération impossible : {}", e));
        } else {
            self.message = None;
        }
        self.refresh(service);
    }

    /// Fenêtre de choix de l'enregistrement à poursuivre.
    fn continue_window(&mut self, ctx: &egui::Context) -> Option<HomeAction> {
        let dialog = self.continue_dialog.as_ref()?;
        let mut chosen = None;
        let mut close = false;
        egui::Window::new(tr("Poursuivre un enregistrement"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(560.0);
                ui.label(tr!("Sauvegarde « {} » (pas {}). L'enregistrement choisi reprend à ce pas ; ses instantanés postérieurs sont supprimés.", dialog.save_name, format::count(dialog.data.state.step)));
                ui.add_space(6.0);
                if dialog.options.is_empty() {
                    ui.label(
                        RichText::new(tr("Aucun enregistrement compatible : il faut les mêmes conditions initiales et un instantané antérieur à la sauvegarde."))
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
                                    .map(|d| d.with_timezone(&chrono::Local).format(tr("%d/%m/%Y %H:%M")).to_string())
                                    .unwrap_or_default();
                                ui.label(
                                    RichText::new(tr!("{} · {} instantanés{} · reprise après le pas {}", date, format::count(c.summary.frame_count as u64), if c.summary.complete { "" } else { " · non clos" }, format::count(c.resume_step)))
                                    .weak(),
                                );
                                if c.dropped_frames > 0 {
                                    ui.label(
                                        RichText::new(tr!("{} instantanés postérieurs seront supprimés", format::count(c.dropped_frames as u64)))
                                        .small()
                                        .color(egui::Color32::from_rgb(255, 120, 120)),
                                    );
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(tr("Poursuivre")).clicked() {
                                    chosen = Some(c.summary.path.clone());
                                }
                            });
                        });
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Annuler")).clicked() {
                        close = true;
                    }
                });
            });
        if let Some(path) = chosen {
            let d = self.continue_dialog.take()?;
            return Some(HomeAction::LoadContinuing(d.data, path, d.save_name));
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
            Err(e) => self.message = Some(tr!("Lecture de la sauvegarde impossible : {}", e)),
        }
    }

    fn new_simulation_form(
        &mut self,
        ui: &mut egui::Ui,
        service: &SimulatorService,
        ic: &InitialConditions,
    ) -> Option<HomeAction> {
        let mut action = None;
        ui.heading(tr("Nouvelle simulation"));
        ui.add_space(4.0);

        egui::Grid::new("ic-grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                param(ui, tr("Nom"), tr("Nom des sauvegardes (F5) et des enregistrements de cette simulation. Par défaut : date, nombre de particules, répartition et vitesse initiale."));
                ui.horizontal(|ui| {
                    if !self.name_edited {
                        let taken = |n: &str| {
                            self.saves.iter().any(|s| name_from_file(&s.name) == n)
                                || self.recordings.iter().any(|r| r.name == n)
                        };
                        self.name = default_name(ic, chrono::Local::now(), taken);
                    }
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.name).desired_width(260.0))
                        .changed()
                    {
                        self.name_edited = true;
                    }
                    if self.name_edited
                        && ui
                            .small_button(tr("Auto"))
                            .on_hover_text(tr("Revenir au nom tiré des paramètres"))
                            .clicked()
                    {
                        self.name_edited = false;
                    }
                });
                ui.end_row();

                param(ui, tr("Nombre de particules"), tr("Nombre de corps au départ, tous de masse 1. Le coût d'un pas croît à peu près comme n·log n ; le maximum proposé dépend du moteur et de la mémoire de la machine."));
                let max = max_bodies(service, &self.engine);
                count_picker(ui, &mut self.count, max);
                ui.end_row();

                param(ui, tr("Rayon du nuage"), tr("Rayon de la sphère ou du disque initial (unité de longueur libre). C'est aussi le rayon de référence du nettoyage."));
                ui.add(
                    egui::DragValue::new(&mut self.radius)
                        .range(1e-3..=1e9)
                        .speed(1.0),
                );
                ui.end_row();

                param(ui, tr("Graine aléatoire"), tr("Mêmes paramètres et même graine : même nuage initial."));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.seed).speed(1.0));
                    if ui.button("🎲").on_hover_text(tr("Nouvelle graine")).clicked() {
                        self.seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64 % 1_000_000_000)
                            .unwrap_or(1);
                    }
                });
                ui.end_row();

                param(ui, tr("Répartition"), tr("Sphère pleine de densité uniforme, ou disque dans le plan (x, y)."));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.disk, false, tr("Sphère"));
                    ui.radio_value(&mut self.disk, true, tr("Disque"));
                });
                ui.end_row();

                if self.disk {
                    param(ui, tr("   Épaisseur (× rayon)"), tr("Épaisseur du disque, en fraction de son rayon."));
                    ui.add(
                        egui::DragValue::new(&mut self.disk_thickness)
                            .range(0.0..=1.0)
                            .speed(0.002),
                    );
                    ui.end_row();
                    param(ui, tr("   Profil de densité"), tr("Densité surfacique uniforme, ou décroissant exponentiellement avec la distance au centre."));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.disk_exponential, false, tr("Uniforme"));
                        ui.radio_value(&mut self.disk_exponential, true, tr("Exponentiel"));
                    });
                    ui.end_row();
                    if self.disk_exponential {
                        param(ui, tr("   Longueur d'échelle (× rayon)"), tr("Distance, en fraction du rayon, sur laquelle la densité du disque est divisée par e."));
                        ui.add(
                            egui::DragValue::new(&mut self.disk_scale)
                                .range(0.01..=2.0)
                                .speed(0.005),
                        );
                        ui.end_row();
                    }
                }

                param(ui, tr("Vitesse initiale"), tr("Repos : effondrement libre. Aléatoire : directions isotropes. Circulaire : rotation autour de Oz, à vitesse imposée ou à la vitesse d'équilibre orbital (calculée sur la masse intérieure). Radiale : chute vers le centre."));
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
                        param(ui, tr("   Dispersion (±)"), tr("Écart aléatoire autour de la vitesse orbitale d'équilibre (± ce pourcentage)."));
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
                        param(ui, tr("   Norme : plage"), tr("Norme de la vitesse, tirée uniformément entre le minimum et le maximum."));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.vel_min)
                                    .range(0.0..=1e9)
                                    .speed(0.1)
                                    .prefix(tr("min ")),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.vel_max)
                                    .range(0.0..=1e9)
                                    .speed(0.1)
                                    .prefix(tr("max ")),
                            );
                        });
                        ui.end_row();
                    }
                }
            });
        ui.label(
            RichText::new(tr!("Vitesse caractéristique √(GM/R) ≈ {} · temps de chute libre ≈ {}", format::real(ic.characteristic_speed(self.g)), format::real(ic.free_fall_time(self.g))))
            .weak(),
        );

        ui.add_space(6.0);
        egui::CollapsingHeader::new(tr("Paramètres avancés"))
            .default_open(false)
            .show(ui, |ui| self.advanced(ui, service, ic));

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.headless, tr("Sans affichage (enregistrement automatique)"));
            help(
                ui,
                tr("Calcule sans dessiner les corps, toute la carte graphique pour le calcul. Un écran de suivi montre l'avancement ; à la cible (ou à l'arrêt), l'enregistrement est clos et l'état final sauvegardé. Aussi en ligne de commande : stellar-cli.exe (stellar-cli --help)."),
            );
        });
        if self.headless {
            ui.horizontal(|ui| {
                ui.label(tr("   Arrêt :"));
                egui::ComboBox::from_id_salt("target")
                    .selected_text(self.target_kind.label())
                    .show_ui(ui, |ui| {
                        for k in TargetKind::ALL {
                            ui.selectable_value(&mut self.target_kind, k, k.label());
                        }
                    });
                match self.target_kind {
                    TargetKind::Manual => {}
                    TargetKind::Steps => {
                        ui.add(
                            egui::DragValue::new(&mut self.target_steps)
                                .range(1..=u64::MAX / 2)
                                .speed(100.0),
                        );
                        ui.label(tr("pas"));
                    }
                    TargetKind::SimTime => {
                        ui.add(
                            egui::DragValue::new(&mut self.target_time)
                                .range(1e-9..=1e12)
                                .speed(0.1),
                        );
                        ui.label(tr!("(≈ {} pas)", format::count((self.target_time / self.physics.dt.max(1e-300)).ceil() as u64)));
                    }
                    TargetKind::WallTime => {
                        ui.add(
                            egui::DragValue::new(&mut self.target_minutes)
                                .range(0.1..=1e6)
                                .speed(1.0),
                        );
                        ui.label(tr("minutes"));
                    }
                }
            });
        } else {
            ui.checkbox(
                &mut self.record,
                tr("Enregistrer dès le départ (pour relecture fluide)"),
            );
        }
        if self.record || self.headless {
            ui.horizontal(|ui| {
                param(ui, tr("   Un instantané tous les"), tr("Intervalle entre deux instantanés enregistrés ; la relecture interpole entre eux."));
                ui.add(egui::DragValue::new(&mut self.record_interval).range(1..=100_000));
                ui.label(tr("pas"));
            });
            ui.horizontal(|ui| {
                ui.label("  ");
                ui.checkbox(&mut self.record_budget_enabled, tr("Poussière limitée à"));
                help(
                    ui,
                    tr("Nombre maximal de particules de masse 1 enregistrées par instantané \
                     (choisies de façon stable) ; les corps issus de fusions sont toujours \
                     gardés."),
                );
                ui.add_enabled(
                    self.record_budget_enabled,
                    egui::DragValue::new(&mut self.record_budget)
                        .range(1_000..=100_000_000)
                        .speed(10_000.0),
                );
                ui.label(tr("particules"));
            });
            let kept = if self.record_budget_enabled {
                self.count.min(self.record_budget as usize)
            } else {
                self.count
            };
            ui.label(
                RichText::new(tr!("≈ {} Mo par instantané au départ (diminue avec les fusions)", format!("{:.0}", kept as f64 * 12.0 / 1e6)))
                .weak(),
            );
        }

        ui.add_space(10.0);
        // Mesuré : ≈ 4,2 Go pour 10 millions de corps (moteur, instantanés, rendu).
        let size = |mb: f64| {
            if mb >= 1000.0 {
                tr!("{} Go", format!("{:.1}", mb / 1000.0)).replace('.', crate::format::decimal_sep())
            } else {
                tr!("{} Mo", format!("{mb:.0}"))
            }
        };
        let mut text = tr!("Mémoire estimée : ≈ {}", size(self.count as f64 * 420.0 / 1e6));
        if self.engine.starts_with("gpu") {
            // Moteur GPU : ≈ 330 octets par corps en mémoire vidéo.
            text.push_str(&tr!(" · mémoire vidéo ≈ {}", size(self.count as f64 * 330.0 / 1e6)));
        }
        ui.label(RichText::new(text).weak());
        let gpu = self.engine.starts_with("gpu");
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr!("Durée estimée du premier pas : ≈ {}", format_duration(step_estimate(self.count, gpu))))
                .weak(),
            );
            help(
                ui,
                tr("Estimation tirée des mesures du banc d'essai (RTX 4060 Ti pour le GPU, processeur de la même machine pour le CPU) ; elle varie avec le matériel, la répartition et θ."),
            );
        });
        if ui
            .add_sized(
                [260.0, 36.0],
                egui::Button::new(
                    RichText::new(if self.headless {
                        tr("▶ Lancer sans affichage")
                    } else {
                        tr("▶ Lancer la simulation")
                    })
                    .size(17.0),
                ),
            )
            .clicked()
        {
            let record = self.record.then_some(RecordOptions {
                interval: self.record_interval.max(1),
                dust_budget: self.record_budget_enabled.then_some(self.record_budget),
            });
            let name = self.name.trim();
            let name = if name.is_empty() {
                default_name(ic, chrono::Local::now(), |_| false)
            } else {
                name.to_owned()
            };
            let config = SimulationConfig {
                initial: *ic,
                physics: self.physics,
                engine: self.engine.clone(),
            };
            action = Some(if self.headless {
                let target = match self.target_kind {
                    TargetKind::Manual => RunTarget::Manual,
                    TargetKind::Steps => RunTarget::Steps(self.target_steps.max(1)),
                    TargetKind::SimTime => RunTarget::SimTime(self.target_time),
                    TargetKind::WallTime => RunTarget::WallTime(
                        std::time::Duration::from_secs_f64(self.target_minutes.max(0.0) * 60.0),
                    ),
                };
                HomeAction::StartHeadless(
                    config,
                    RecordSettings {
                        interval: self.record_interval.max(1),
                        dust_budget: self.record_budget_enabled.then_some(self.record_budget),
                    },
                    name,
                    target,
                )
            } else {
                HomeAction::Start(config, record, name)
            });
        }
        action
    }

    fn advanced(&mut self, ui: &mut egui::Ui, service: &SimulatorService, ic: &InitialConditions) {
        ui.horizontal(|ui| {
            param(ui, tr("Constante G"), tr("Constante de gravitation (unités libres)."));
            ui.add(
                egui::DragValue::new(&mut self.g)
                    .range(1e-12..=1e12)
                    .speed(0.01),
            );
        });
        ui.checkbox(
            &mut self.auto_physics,
            tr("Déduire automatiquement dt, ε, r₀ et la liaison des amas des conditions initiales"),
        );
        if !self.auto_physics && ui.button(tr("Recalculer les valeurs suggérées")).clicked() {
            self.physics = PhysicsParams::suggested(ic, self.g);
        }
        self.physics.g = self.g;

        ui.add_enabled_ui(!self.auto_physics, |ui| {
            egui::Grid::new("phys-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    let p = &mut self.physics;
                    param(ui, tr("Pas de temps dt"), tr("Pas de temps fixe de l'intégrateur (Leapfrog). Plus petit : plus précis, mais plus lent. Par défaut : temps de chute libre / 500."));
                    let sp = p.dt * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.dt)
                            .range(1e-12..=1e6)
                            .speed(sp)
                            .max_decimals(8),
                    );
                    ui.end_row();
                    param(ui, tr("Adoucissement ε"), tr("Adoucissement de Plummer : borne l'attraction lors des rencontres très proches. Par défaut : 0,3 × distance moyenne entre voisins."));
                    let sp = p.softening.max(1e-6) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.softening)
                            .range(0.0..=1e9)
                            .speed(sp)
                            .max_decimals(6),
                    );
                    ui.end_row();
                    param(ui, tr("Rayon r₀ (masse 1)"), tr("Rayon d'un corps de masse 1 (r = r₀ · m^⅓) : il décide des contacts et donc des fusions. 0 : aucune fusion."));
                    let sp = p.base_radius.max(1e-6) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.base_radius)
                            .range(0.0..=1e9)
                            .speed(sp)
                            .max_decimals(6),
                    );
                    ui.end_row();
                    param(ui, tr("Amas éjectés : liaison"), tr("Deux corps lointains plus proches que cette distance appartiennent au même groupe (par défaut : distance moyenne entre voisins au départ)."));
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
                param(ui, tr("Précision Barnes-Hut θ"), tr("Critère d'ouverture : un groupe de corps vu sous un angle inférieur à θ est remplacé par son centre de masse. Plus petit : plus précis, mais plus lent."));
                ui.add(egui::Slider::new(&mut p.theta, 0.1..=1.0));
                ui.end_row();
                param(ui, tr("Nettoyage : période (pas)"), tr("Tous les combien de pas les corps éjectés sont éliminés (0 : jamais)."));
                ui.add(egui::DragValue::new(&mut p.cleanup_period).range(0..=1_000_000));
                ui.end_row();
                param(ui, tr("Nettoyage : distance (× rayon)"), tr("Seuls les corps non liés situés au-delà de cette distance du barycentre (en rayons du nuage) sont éliminés ; les amas éjectés liés sont conservés."));
                ui.add(
                    egui::DragValue::new(&mut p.cleanup_distance_factor)
                        .range(1.0..=1e6)
                        .speed(0.1),
                );
                ui.end_row();
                param(ui, tr("Amas éjectés : corps au moins"), tr("Un amas éjecté lié d'au moins ce nombre de corps est conservé par le nettoyage (0 : critère ignoré)."));
                ui.add(egui::DragValue::new(&mut p.cluster_min_bodies).range(0..=10_000_000));
                ui.end_row();
                param(ui, tr("… ou masse au moins"), tr("… ou d'au moins cette part de la masse initiale, même s'il ne compte qu'un corps (0 : critère ignoré)."));
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
                param(ui, tr("Moteur de calcul"), tr("GPU : carte graphique, simple précision, bien plus rapide. CPU : tous les cœurs sauf un, double précision."));
                let engines = service.engines();
                let current = engines
                    .iter()
                    .find(|e| e.id == self.engine)
                    .map(|e| tr(e.label))
                    .unwrap_or("?");
                egui::ComboBox::from_id_salt("engine")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for e in &engines {
                            ui.selectable_value(&mut self.engine, e.id.to_owned(), tr(e.label))
                                .on_hover_text(tr(e.description));
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
            ui.heading(tr("Simulations sauvegardées"));
            if ui.button("⟳").on_hover_text(tr("Actualiser")).clicked() {
                self.refresh(service);
            }
            if ui.button(tr("Parcourir…")).clicked() {
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
            ui.label(RichText::new(tr("Aucune sauvegarde dans le dossier « saves ». Appuyez sur F5 pendant une simulation.")).weak());
        }
        let mut to_continue = None;
        let mut file_op: Option<(bool, FileTarget)> = None;
        for s in &self.saves {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&s.name).strong());
                        let date = chrono::DateTime::from_timestamp(s.saved_at_unix, 0)
                            .map(|d| {
                                d.with_timezone(&chrono::Local)
                                    .format(tr("%d/%m/%Y %H:%M"))
                                    .to_string()
                            })
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(tr!("{} · {} corps · pas {} · t = {}", date, format::count(s.body_count as u64), format::count(s.step), format::real(s.time)))
                            .weak(),
                        );
                        if let Some(rec) = &s.recording {
                            ui.label(
                                RichText::new(tr!("Reprend l'enregistrement « {} »", rec))
                                    .small()
                                    .color(egui::Color32::from_rgb(255, 120, 120)),
                            );
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if ui.button(tr("Charger")).clicked() {
                        action = Some(HomeAction::Load(s.path.clone()));
                    }
                    if ui
                        .button(tr("Poursuivre un enregistrement…"))
                        .on_hover_text(tr("Charger cette sauvegarde et prolonger un enregistrement existant à partir de son pas"))
                        .clicked()
                    {
                        to_continue = Some(s.clone());
                    }
                    if ui.small_button(tr("Renommer…")).clicked() {
                        file_op = Some((false, FileTarget::Save(s.clone())));
                    }
                    if ui.small_button(tr("Supprimer…")).clicked() {
                        file_op = Some((true, FileTarget::Save(s.clone())));
                    }
                });
            });
        }
        if let Some(s) = to_continue {
            self.open_continue_dialog(service, &s);
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.heading(tr("Enregistrements"));
            if ui.button(tr("Parcourir…")).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Enregistrement Stellar", &["stellarec"])
                    .pick_file()
                {
                    action = Some(HomeAction::Replay(path));
                }
            }
        });
        if self.recordings.is_empty() {
            ui.label(RichText::new(tr("Aucun enregistrement dans le dossier « recordings ». Appuyez sur F9 pendant une simulation.")).weak());
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
                            .map(|d| d.with_timezone(&chrono::Local).format(tr("%d/%m/%Y %H:%M")).to_string())
                            .unwrap_or_default();
                        let size = if r.bytes >= 1_000_000_000 {
                            tr!("{} Go", format!("{:.2}", r.bytes as f64 / 1e9))
                        } else {
                            tr!("{} Mo", format!("{:.0}", r.bytes as f64 / 1e6))
                        }
                        .replace('.', crate::format::decimal_sep());
                        ui.label(
                            RichText::new(tr!("{} · {} instantanés · {} corps au départ · durée {} · {}{}", date, format::count(r.frame_count as u64), format::count(r.first_count as u64), format::real(r.duration), size, if r.complete { "" } else { " · non clos (récupéré)" }))
                            .weak(),
                        );
                    });
                });
                ui.horizontal(|ui| {
                    if ui.button(tr("Rejouer")).clicked() {
                        action = Some(HomeAction::Replay(r.path.clone()));
                    }
                    // Sauvegarde de reprise la plus récente liée à cet enregistrement.
                    let file = r.path.file_name().map(|n| n.to_string_lossy().into_owned());
                    if let Some(save) = self.saves.iter().find(|s| s.recording.is_some() && s.recording == file) {
                        if ui
                            .button(tr("Reprendre"))
                            .on_hover_text(tr!("Recharge la sauvegarde « {} » (pas {}) et poursuit l'enregistrement", save.name, format::count(save.step)))
                            .clicked()
                        {
                            action = Some(HomeAction::Load(save.path.clone()));
                        }
                    }
                    if ui.small_button(tr("Renommer…")).clicked() {
                        file_op = Some((false, FileTarget::Recording(r.clone())));
                    }
                    if ui.small_button(tr("Supprimer…")).clicked() {
                        file_op = Some((true, FileTarget::Recording(r.clone())));
                    }
                });
            });
        }
        match file_op {
            Some((true, target)) => self.ask_delete(service, target),
            Some((false, target)) => self.ask_rename(service, target),
            None => {}
        }
        action
    }
}

/// Octets de mémoire centrale par corps (mesuré : ≈ 4,2 Go pour 10 millions
/// de corps, moteur, instantanés et rendu compris).
const RAM_PER_BODY: f64 = 420.0;
/// Plafond du nombre de corps proposé.
const MAX_COUNT: usize = 1_000_000_000;

/// Mémoire centrale totale de la machine (octets), si elle est connue.
#[cfg(windows)]
fn total_memory() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY : structure initialisée, de taille renseignée.
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys)
}

#[cfg(not(windows))]
fn total_memory() -> Option<u64> {
    None
}

/// Nombre maximal de corps proposé : limite du moteur, 80 % de la mémoire
/// centrale, un milliard au plus.
fn max_bodies(service: &SimulatorService, engine: &str) -> usize {
    let by_ram = total_memory().map_or(usize::MAX, |m| (m as f64 * 0.8 / RAM_PER_BODY) as usize);
    service
        .max_bodies(engine)
        .unwrap_or(usize::MAX)
        .min(by_ram)
        .clamp(1_000, MAX_COUNT)
}

/// Crans 1-2-5 de 1 000 à `max` (inclus, ajouté s'il n'est pas un cran).
pub fn count_steps(max: usize) -> Vec<usize> {
    let mut steps = Vec::new();
    let mut decade = 1_000usize;
    'outer: loop {
        for m in [1, 2, 5] {
            let v = decade * m;
            if v > max {
                break 'outer;
            }
            steps.push(v);
        }
        decade *= 10;
    }
    if steps.last() != Some(&max) {
        steps.push(max);
    }
    steps
}

/// Curseur logarithmique (crans 1-2-5) et saisie exacte du nombre de corps.
fn count_picker(ui: &mut egui::Ui, count: &mut usize, max: usize) {
    *count = (*count).clamp(2, max);
    let steps = count_steps(max);
    // Cran le plus proche par valeur inférieure ou égale.
    let mut index = steps.iter().rposition(|&s| s <= *count).unwrap_or(0);
    ui.horizontal(|ui| {
        let slider = egui::Slider::new(&mut index, 0..=steps.len() - 1).show_value(false);
        if ui
            .add(slider)
            .on_hover_text(tr!("Maximum sur cette machine : {}", format::count(max as u64)))
            .changed()
        {
            *count = steps[index];
        }
        let speed = (*count as f64 * 0.01).max(1.0);
        ui.add(egui::DragValue::new(count).range(2..=max).speed(speed))
        .on_hover_text(tr("Valeur exacte"));
    });
}

/// Pastille « ? » dont la bulle explique un paramètre.
fn help(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new("?").small().strong().weak()).sense(egui::Sense::hover()))
        .on_hover_text(text);
}

/// Libellé d'un paramètre suivi de sa pastille d'aide.
fn param(ui: &mut egui::Ui, label: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        help(ui, text);
    });
}

/// Durée estimée d'un pas (secondes), d'après le banc d'essai : GPU, 2,2 ms
/// à 100 000 corps, 14 ms à 1 M, 62 ms à 4 M ; CPU, 232 ms à 1 M, en
/// n·log n.
pub fn step_estimate(count: usize, gpu: bool) -> f64 {
    let n = count.max(2) as f64;
    if gpu {
        0.9e-3 + 13.5e-3 * (n / 1e6).powf(1.055)
    } else {
        0.2e-3 + 0.232 * (n / 1e6) * n.log2() / 1e6f64.log2()
    }
}

/// Durée lisible : millisecondes, secondes ou minutes.
fn format_duration(seconds: f64) -> String {
    let text = if seconds < 1.0 {
        format!("{:.1} ms", seconds * 1e3)
    } else if seconds < 120.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{:.0} min", seconds / 60.0)
    };
    text.replace('.', crate::format::decimal_sep())
}

/// Nombre abrégé : 1k, 2,5M, 1G…
fn abbreviate(n: usize) -> String {
    let (unit, scale) = match n {
        n if n >= 1_000_000_000 => ("G", 1e9),
        n if n >= 1_000_000 => ("M", 1e6),
        n if n >= 1_000 => ("k", 1e3),
        _ => ("", 1.0),
    };
    let v = n as f64 / scale;
    let text = if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    };
    format!("{}{unit}", text.replace('.', crate::format::decimal_sep()))
}

/// Nom par défaut d'une simulation : date, nombre de particules abrégé,
/// répartition et vitesse initiale (`2026-10-28_1M_sph_orb`) ; l'heure est
/// ajoutée si `taken` indique que le nom existe déjà.
pub fn default_name(
    ic: &InitialConditions,
    now: chrono::DateTime<chrono::Local>,
    taken: impl Fn(&str) -> bool,
) -> String {
    let dist = match ic.distribution {
        Distribution::Sphere => "sph",
        Distribution::Disk { .. } => "disq",
    };
    let vel = match ic.velocity {
        VelocityMode::Rest => "repos",
        VelocityMode::Random { .. } => "alea",
        VelocityMode::CircularFixed { .. } => "circ",
        VelocityMode::CircularOrbital { .. } => "orb",
        VelocityMode::Radial { .. } => "rad",
    };
    let tail = format!("{}_{dist}_{vel}", abbreviate(ic.count));
    let short = format!("{}_{tail}", now.format("%Y-%m-%d"));
    if taken(&short) {
        format!("{}_{tail}", now.format("%Y-%m-%d_%Hh%M"))
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_names() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 28, 14, 5, 0).unwrap();
        let ic = InitialConditions {
            count: 1_000_000,
            velocity: VelocityMode::CircularOrbital { spread: 0.05 },
            ..Default::default()
        };
        assert_eq!(default_name(&ic, now, |_| false), "2026-10-28_1M_sph_orb");
        assert_eq!(default_name(&ic, now, |_| true), "2026-10-28_14h05_1M_sph_orb");
        assert_eq!(abbreviate(2_500), "2,5k");
        assert_eq!(abbreviate(16_776_960), "16,8M");
        assert_eq!(abbreviate(250_000), "250k");
        assert_eq!(abbreviate(1_000_000_000), "1G");
        assert_eq!(abbreviate(512), "512");
    }

    #[test]
    fn count_steps_follow_1_2_5_up_to_the_maximum() {
        assert_eq!(count_steps(10_000), vec![1_000, 2_000, 5_000, 10_000]);
        let s = count_steps(16_776_960);
        assert_eq!(s[..4], [1_000, 2_000, 5_000, 10_000]);
        assert_eq!(s[s.len() - 2..], [10_000_000, 16_776_960]);
        assert_eq!(*count_steps(1_000_000_000).last().unwrap(), 1_000_000_000);
    }

    #[test]
    fn step_estimates_match_bench_measurements() {
        let ms = |n, gpu| step_estimate(n, gpu) * 1e3;
        assert!((ms(100_000, true) - 2.2).abs() < 0.3);
        assert!((ms(1_000_000, true) - 14.4).abs() < 1.0);
        assert!((ms(4_000_000, true) - 62.1).abs() < 4.0);
        assert!((ms(1_000_000, false) - 232.0).abs() < 1.0);
        assert_eq!(format_duration(0.0144), "14,4 ms");
        assert_eq!(format_duration(3.25), "3,2 s");
    }
}

