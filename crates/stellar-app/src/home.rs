//! Home screen: creating a simulation or loading a save.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use stellar_application::{
    name_from_file, Continuation, RecordSettings, RecordingSummary, RunTarget, SaveData,
    SaveSummary, SimulatorService,
};
use stellar_domain::{
    DensityProfile, Distribution, DustMasses, InitialConditions, PhysicsParams, Planets,
    SimulationConfig, VelocityMode, MAX_DUST_MASS, MAX_MASSIVE_FRACTION, MAX_PLANETS,
};

use crate::format;

use crate::errors::Localized;
use crate::i18n::tr;

/// Recording options chosen at creation.
#[derive(Clone, Copy)]
pub struct RecordOptions {
    pub interval: u32,
    pub dust_budget: Option<u64>,
}

pub enum HomeAction {
    None,
    /// Configuration, recording from the start, name of the simulation.
    Start(SimulationConfig, Option<RecordOptions>, String),
    /// Simulation without display: configuration, recording, name, target.
    /// Last field: automatic removal of negligible lone bodies.
    StartHeadless(SimulationConfig, RecordSettings, String, RunTarget, bool),
    Load(PathBuf),
    /// Loads a save already read (name of the save) and continues the given
    /// recording.
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
            VelKind::Rest => tr("At rest"),
            VelKind::Random => tr("Random (isotropic)"),
            VelKind::CircularFixed => tr("Circular — set speed"),
            VelKind::CircularOrbital => tr("Circular — orbital speed"),
            VelKind::Radial => tr("Radial, towards the centre"),
        }
    }
}

pub struct HomeScreen {
    /// Name of the new simulation; until it is typed, it follows the parameters
    /// ([`default_name`]).
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
    /// Multiple of the orbital equilibrium speed.
    vel_factor: f64,
    dust: DustMasses,
    sun: bool,
    /// Share of the sun in the total mass.
    sun_share: f64,
    planets_enabled: bool,
    planets: Planets,
    g: f64,
    auto_physics: bool,
    physics: PhysicsParams,
    engine: String,
    saves: Vec<SaveSummary>,
    saves_error: Option<String>,
    recordings: Vec<RecordingSummary>,
    record: bool,
    /// Without display (automatic recording), and its target.
    headless: bool,
    /// Without display: automatic removal of negligible lone bodies.
    headless_prune: bool,
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

/// File targeted by a deletion or a rename.
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

/// Confirmation of a deletion, or input of a new name.
enum FileDialog {
    Delete {
        target: FileTarget,
        /// Linked files (names), offered for deletion.
        linked: Vec<String>,
        with_linked: bool,
    },
    Rename {
        target: FileTarget,
        name: String,
        /// Linked files renamed along with it.
        linked: Vec<String>,
    },
}

/// Stop condition of a simulation without display.
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
            TargetKind::Manual => tr("manual (stop button)"),
            TargetKind::Steps => tr("after a number of steps"),
            TargetKind::SimTime => tr("after a simulated time"),
            TargetKind::WallTime => tr("after a real duration"),
        }
    }
}

/// Choice of the recording to continue from a save.
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
            vel_factor: 1.0,
            dust: ic.dust,
            sun: false,
            sun_share: 0.5,
            planets_enabled: false,
            planets: Planets {
                count: 5,
                ..ic.planets
            },
            g: 1.0,
            auto_physics: true,
            physics: PhysicsParams::suggested(&ic, 1.0),
            engine: service.default_engine().unwrap_or_default().to_owned(),
            saves: Vec::new(),
            saves_error: None,
            recordings: Vec::new(),
            record: false,
            headless: false,
            headless_prune: false,
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
            Err(e) => self.saves_error = Some(e.localized()),
        }
        match service.list_recordings() {
            Ok(list) => self.recordings = list,
            Err(e) => self.saves_error = Some(e.localized()),
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
                factor: self.vel_factor,
            },
            VelKind::Radial => VelocityMode::Radial { min, max },
        };
        InitialConditions {
            count: self.count,
            radius: self.radius,
            seed: self.seed,
            distribution,
            velocity,
            dust: self.dust,
            sun: self.sun.then_some(self.sun_share),
            planets: Planets {
                count: if self.planets_enabled { self.planets.count } else { 0 },
                ..self.planets
            },
        }
    }

    /// Rows of the creation grid: dust mass spectrum, sun, massive bodies.
    fn massive_bodies_rows(&mut self, ui: &mut egui::Ui) {
        param(ui, tr("Grain mass"), tr("Maximum mass of the dust grains: their masses are integers from 1 to this maximum (1: all of mass 1)."));
        ui.add(
            egui::DragValue::new(&mut self.dust.max)
                .range(1..=MAX_DUST_MASS)
                .speed(0.2)
                .prefix(tr("from 1 to ")),
        );
        ui.end_row();
        if self.dust.max > 1 {
            param(ui, tr("   Exponent"), tr("Power law of the masses: density ∝ m^exponent. −2: many small grains and few big ones; 0: equally likely masses."));
            ui.add(
                egui::DragValue::new(&mut self.dust.exponent)
                    .range(-4.0..=2.0)
                    .speed(0.02)
                    .fixed_decimals(2),
            );
            ui.end_row();
        }

        param(ui, tr("Central sun"), tr("Body at rest at the center of the cloud, whose mass is a share of the total mass (sun included)."));
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.sun, "");
            if self.sun {
                share_input(ui, &mut self.sun_share, 0.01, MAX_MASSIVE_FRACTION, tr(" % of the total mass"));
            }
        });
        ui.end_row();

        param(ui, tr("Massive bodies"), tr("Protoplanets on circular orbits in the (x, y) plane, whatever the velocity mode of the grains, at evenly spaced radii and random angles."));
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.planets_enabled, "");
            if self.planets_enabled {
                ui.add(
                    egui::DragValue::new(&mut self.planets.count)
                        .range(1..=MAX_PLANETS)
                        .speed(0.1),
                );
            }
        });
        ui.end_row();
        if self.planets_enabled {
            param(ui, tr("   Mass of each"), tr("Mass of each massive body, as a share of the total mass."));
            share_input(ui, &mut self.planets.mass_fraction, 1e-5, 0.5, tr(" % of the total mass"));
            ui.end_row();
            param(ui, tr("   Orbits (× radius)"), tr("Radii of the first and last orbits, as a fraction of the cloud radius; the others are evenly spaced in between."));
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.planets.inner)
                        .range(0.01..=self.planets.outer)
                        .speed(0.005)
                        .prefix(tr("from ")),
                );
                ui.add(
                    egui::DragValue::new(&mut self.planets.outer)
                        .range(self.planets.inner..=2.0)
                        .speed(0.005)
                        .prefix(tr("to ")),
                );
            });
            ui.end_row();
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
                        tr("Stellar — gravitational N-body simulator (CPU or GPU computation, GPU rendering)"),
                    )
                    .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("Quit")).clicked() {
                        action = HomeAction::Quit;
                    }
                    crate::i18n::picker(ui);
                    ui.label(tr("Language"));
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.heading(RichText::new("STELLAR").size(34.0).strong());
                ui.label(
                    RichText::new(tr("Gravitational collapse and particle mergers")).italics(),
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

    /// Names of the files linked to `target`: the recording of a save, the saves
    /// of a recording.
    fn linked_names(service: &SimulatorService, target: &FileTarget) -> Vec<String> {
        match target {
            FileTarget::Save(s) => service
                .linked_recording(s)
                .map(|r| tr!("recording “{}”", r.name))
                .into_iter()
                .collect(),
            FileTarget::Recording(r) => service
                .linked_saves(&r.path)
                .into_iter()
                .map(|s| tr!("save “{}”", s.name))
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

    /// Confirmation window (deletion) or input window (rename).
    fn file_window(&mut self, ctx: &egui::Context, service: &SimulatorService) {
        let Some(dialog) = &mut self.file_dialog else {
            return;
        };
        let (mut confirmed, mut cancelled) = (false, false);
        let title = match dialog {
            FileDialog::Delete { .. } => tr("Delete"),
            FileDialog::Rename { .. } => tr("Rename"),
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
                            FileTarget::Save(_) => tr("the save"),
                            FileTarget::Recording(_) => tr("the recording"),
                        };
                        ui.label(tr!("Delete {} “{}”?", what, target.name()));
                        ui.label(RichText::new(tr("The file goes to the Windows Recycle Bin.")).weak());
                        if !linked.is_empty() {
                            ui.checkbox(
                                with_linked,
                                tr!("Also delete: {}", linked.join(", ")),
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
                                RichText::new(tr!("Renamed along with it: {}.", linked.join(", ")))
                                    .weak(),
                            );
                        }
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = match dialog {
                        FileDialog::Delete { .. } => tr("Delete"),
                        FileDialog::Rename { .. } => tr("Rename"),
                    };
                    confirmed |= ui.button(ok).clicked();
                    cancelled |= ui.button(tr("Cancel")).clicked();
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
            self.message = Some(tr!("Operation failed: {}", e.localized()));
        } else {
            self.message = None;
        }
        self.refresh(service);
    }

    /// Window to choose the recording to continue.
    fn continue_window(&mut self, ctx: &egui::Context) -> Option<HomeAction> {
        let dialog = self.continue_dialog.as_ref()?;
        let mut chosen = None;
        let mut close = false;
        egui::Window::new(tr("Continue a recording"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(560.0);
                ui.label(tr!("Save “{}” (step {}). The chosen recording resumes at this step; its later snapshots are deleted.", dialog.save_name, format::count(dialog.data.state.step)));
                ui.add_space(6.0);
                if dialog.options.is_empty() {
                    ui.label(
                        RichText::new(tr("No compatible recording: it needs the same initial conditions and a snapshot earlier than the save."))
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
                                    .map(|d| d.with_timezone(&chrono::Local).format(tr("%Y-%m-%d %H:%M")).to_string())
                                    .unwrap_or_default();
                                ui.label(
                                    RichText::new(tr!("{} · {} snapshots{} · resumes after step {}", date, format::count(c.summary.frame_count as u64), if c.summary.complete { "" } else { tr(" · unfinished") }, format::count(c.resume_step)))
                                    .weak(),
                                );
                                if c.dropped_frames > 0 {
                                    ui.label(
                                        RichText::new(tr!("{} later snapshots will be deleted", format::count(c.dropped_frames as u64)))
                                        .small()
                                        .color(egui::Color32::from_rgb(255, 120, 120)),
                                    );
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(tr("Continue")).clicked() {
                                    chosen = Some(c.summary.path.clone());
                                }
                            });
                        });
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Cancel")).clicked() {
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
            Err(e) => self.message = Some(tr!("Could not read the save: {}", e.localized())),
        }
    }

    fn new_simulation_form(
        &mut self,
        ui: &mut egui::Ui,
        service: &SimulatorService,
        ic: &InitialConditions,
    ) -> Option<HomeAction> {
        let mut action = None;
        ui.heading(tr("New simulation"));
        ui.add_space(4.0);

        egui::Grid::new("ic-grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                param(ui, tr("Name"), tr("Name of this simulation's saves (F5) and recordings. Default: date, particle count, distribution and initial velocity."));
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
                            .on_hover_text(tr("Go back to the name derived from the parameters"))
                            .clicked()
                    {
                        self.name_edited = false;
                    }
                });
                ui.end_row();

                param(ui, tr("Number of particles"), tr("Number of dust grains at the start (their mass is set below). The cost of a step grows roughly as n·log n; the maximum offered depends on the engine and on the machine's memory."));
                // The sun and the massive bodies count in the engine's limit.
                let massive = usize::from(self.sun) + if self.planets_enabled { self.planets.count as usize } else { 0 };
                let max = max_bodies(service, &self.engine).saturating_sub(massive).max(1_000);
                count_picker(ui, &mut self.count, max);
                ui.end_row();

                param(ui, tr("Cloud radius"), tr("Radius of the initial sphere or disc (free length unit). It is also the reference radius of the clean-up."));
                ui.add(
                    egui::DragValue::new(&mut self.radius)
                        .range(1e-3..=1e9)
                        .speed(1.0),
                );
                ui.end_row();

                param(ui, tr("Random seed"), tr("Same parameters and same seed: same initial cloud."));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.seed).speed(1.0));
                    if ui.button("🎲").on_hover_text(tr("New seed")).clicked() {
                        self.seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64 % 1_000_000_000)
                            .unwrap_or(1);
                    }
                });
                ui.end_row();

                param(ui, tr("Distribution"), tr("Solid sphere of uniform density, or disc in the (x, y) plane."));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.disk, false, tr("Sphere"));
                    ui.radio_value(&mut self.disk, true, tr("Disc"));
                });
                ui.end_row();

                if self.disk {
                    param(ui, tr("   Thickness (× radius)"), tr("Thickness of the disc, as a fraction of its radius."));
                    ui.add(
                        egui::DragValue::new(&mut self.disk_thickness)
                            .range(0.0..=1.0)
                            .speed(0.002),
                    );
                    ui.end_row();
                    param(ui, tr("   Density profile"), tr("Uniform surface density, or decreasing exponentially with the distance to the centre."));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.disk_exponential, false, tr("Uniform"));
                        ui.radio_value(&mut self.disk_exponential, true, tr("Exponential"));
                    });
                    ui.end_row();
                    if self.disk_exponential {
                        param(ui, tr("   Scale length (× radius)"), tr("Distance, as a fraction of the radius, over which the disc density is divided by e."));
                        ui.add(
                            egui::DragValue::new(&mut self.disk_scale)
                                .range(0.01..=2.0)
                                .speed(0.005),
                        );
                        ui.end_row();
                    }
                }

                param(ui, tr("Initial velocity"), tr("At rest: free collapse. Random: isotropic directions. Circular: rotation about Oz, at a set speed or at the orbital equilibrium speed (computed from the enclosed mass). Radial: fall towards the centre."));
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
                        param(ui, tr("   Spread (±)"), tr("Random deviation around the orbital equilibrium speed (± this percentage)."));
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
                        param(ui, tr("   Factor"), tr("Multiple of the equilibrium orbital speed: 1 gives circular orbits, less makes the cloud collapse, more makes it expand."));
                        ui.add(
                            egui::Slider::new(&mut self.vel_factor, 0.5..=1.5)
                                .step_by(0.01)
                                .custom_formatter(|v, _| {
                                    format!("× {v:.2}").replace('.', crate::format::decimal_sep())
                                })
                                .custom_parser(|s| {
                                    s.trim_start_matches(['×', ' ']).replace(',', ".").parse().ok()
                                }),
                        );
                        ui.end_row();
                    }
                    _ => {
                        param(ui, tr("   Magnitude: range"), tr("Speed magnitude, drawn uniformly between the minimum and the maximum."));
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

                self.massive_bodies_rows(ui);
            });
        if !ic.masses_are_valid() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 120, 90),
                tr!("The sun and the massive bodies exceed {} of the total mass.", share_text(MAX_MASSIVE_FRACTION)),
            );
        }
        ui.label(
            RichText::new(tr!("Characteristic speed √(GM/R) ≈ {} · free-fall time ≈ {}", format::real(ic.characteristic_speed(self.g)), format::real(ic.free_fall_time(self.g))))
            .weak(),
        );
        if ic.dust.max > 1 || ic.sun.is_some() || ic.planets.count > 0 {
            ui.label(
                RichText::new(tr!("Total mass ≈ {} (mean grain mass ≈ {})", format::real(ic.expected_total_mass()), format::real(ic.dust.mean())))
                .weak(),
            );
        }

        ui.add_space(6.0);
        egui::CollapsingHeader::new(tr("Advanced parameters"))
            .default_open(false)
            .show(ui, |ui| self.advanced(ui, service, ic));

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.headless, tr("Without display (automatic recording)"));
            help(
                ui,
                tr("Computes without drawing the bodies, the whole graphics card for the computation. A monitoring screen shows the progress; at the target (or when stopped), the recording is closed and the final state saved. Also from the command line: stellar-cli.exe (stellar-cli --help)."),
            );
        });
        if self.headless {
            ui.horizontal(|ui| {
                ui.label(tr("   Stop:"));
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
                        ui.label(tr("steps"));
                    }
                    TargetKind::SimTime => {
                        ui.add(
                            egui::DragValue::new(&mut self.target_time)
                                .range(1e-9..=1e12)
                                .speed(0.1),
                        );
                        ui.label(tr!("(≈ {} steps)", format::count((self.target_time / self.physics.dt.max(1e-300)).ceil() as u64)));
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
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut self.headless_prune,
                    tr("   Automatically remove the negligible lone bodies"),
                );
                help(ui, crate::sim::prune_help());
            });
        } else {
            ui.checkbox(
                &mut self.record,
                tr("Record from the start (for smooth playback)"),
            );
        }
        if self.record || self.headless {
            ui.horizontal(|ui| {
                param(ui, tr("   One snapshot every"), tr("Interval between two recorded snapshots; playback interpolates between them."));
                ui.add(egui::DragValue::new(&mut self.record_interval).range(1..=100_000));
                ui.label(tr("steps"));
            });
            ui.horizontal(|ui| {
                ui.label("  ");
                ui.checkbox(&mut self.record_budget_enabled, tr("Dust limited to"));
                help(
                    ui,
                    tr("Maximum number of mass-1 particles recorded per snapshot (chosen consistently); bodies resulting from mergers are always kept."),
                );
                ui.add_enabled(
                    self.record_budget_enabled,
                    egui::DragValue::new(&mut self.record_budget)
                        .range(1_000..=100_000_000)
                        .speed(10_000.0),
                );
                ui.label(tr("particles"));
            });
            let kept = if self.record_budget_enabled {
                self.count.min(self.record_budget as usize)
            } else {
                self.count
            };
            ui.label(
                RichText::new(tr!("≈ {} MB per snapshot at the start (decreases with mergers)", format!("{:.0}", kept as f64 * 12.0 / 1e6)))
                .weak(),
            );
        }

        ui.add_space(10.0);
        // Measured: ≈ 4.2 GB for 10 million bodies (engine, snapshots, rendering).
        let size = |mb: f64| {
            if mb >= 1000.0 {
                tr!("{} GB", format!("{:.1}", mb / 1000.0)).replace('.', crate::format::decimal_sep())
            } else {
                tr!("{} MB", format!("{mb:.0}"))
            }
        };
        let mut text = tr!("Estimated memory: ≈ {}", size(self.count as f64 * 420.0 / 1e6));
        if self.engine.starts_with("gpu") {
            // GPU engine, measured with the benchmark from 0.5 to 4 M bodies:
            // ≈ 300 bytes of video memory per body plus ≈ 130 MB fixed.
            let video = 130.0 + self.count as f64 * 300.0 / 1e6;
            text.push_str(&tr!(" · video memory ≈ {}", size(video)));
        }
        ui.label(RichText::new(text).weak());
        let gpu = self.engine.starts_with("gpu");
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr!("Estimated duration of the first step: ≈ {}", format_duration(step_estimate(self.count, gpu))))
                .weak(),
            );
            help(
                ui,
                tr("Estimate based on benchmark measurements (RTX 4060 Ti for the GPU, the same machine's processor for the CPU); it varies with the hardware, the distribution and θ."),
            );
        });
        if ui
            .add_enabled_ui(ic.masses_are_valid(), |ui| {
                ui.add_sized(
                    [260.0, 36.0],
                    egui::Button::new(
                        RichText::new(if self.headless {
                            tr("▶ Start without display")
                        } else {
                            tr("▶ Start the simulation")
                        })
                        .size(17.0),
                    ),
                )
            })
            .inner
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
                    self.headless_prune,
                )
            } else {
                HomeAction::Start(config, record, name)
            });
        }
        action
    }

    fn advanced(&mut self, ui: &mut egui::Ui, service: &SimulatorService, ic: &InitialConditions) {
        ui.horizontal(|ui| {
            param(ui, tr("Constant G"), tr("Gravitational constant (free units)."));
            ui.add(
                egui::DragValue::new(&mut self.g)
                    .range(1e-12..=1e12)
                    .speed(0.01),
            );
        });
        ui.checkbox(
            &mut self.auto_physics,
            tr("Derive dt, ε, r₀ and the cluster linking length automatically from the initial conditions"),
        );
        if !self.auto_physics && ui.button(tr("Recompute the suggested values")).clicked() {
            self.physics = PhysicsParams::suggested(ic, self.g);
        }
        self.physics.g = self.g;

        ui.add_enabled_ui(!self.auto_physics, |ui| {
            egui::Grid::new("phys-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    let p = &mut self.physics;
                    param(ui, tr("Time step dt"), tr("Fixed time step of the (Leapfrog) integrator. Smaller: more accurate, but slower. Default: free-fall time / 500."));
                    let sp = p.dt * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.dt)
                            .range(1e-12..=1e6)
                            .speed(sp)
                            .max_decimals(8),
                    );
                    ui.end_row();
                    param(ui, tr("Softening ε"), tr("Plummer softening: bounds the attraction during very close encounters. Default: 0.3 × mean distance between neighbours."));
                    let sp = p.softening.max(1e-6) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.softening)
                            .range(0.0..=1e9)
                            .speed(sp)
                            .max_decimals(6),
                    );
                    ui.end_row();
                    param(ui, tr("Radius r₀ (mass 1)"), tr("Radius of a body of mass 1 (r = r₀ · m^⅓): it decides contacts and hence mergers. 0: no mergers."));
                    let sp = p.base_radius.max(1e-6) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut p.base_radius)
                            .range(0.0..=1e9)
                            .speed(sp)
                            .max_decimals(6),
                    );
                    ui.end_row();
                    param(ui, tr("Ejected clusters: linking length"), tr("Two distant bodies closer than this distance belong to the same group (default: mean distance between neighbours at the start)."));
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
                param(ui, tr("Barnes-Hut accuracy θ"), tr("Opening criterion: a group of bodies seen under an angle smaller than θ is replaced by its centre of mass. Smaller: more accurate, but slower."));
                ui.add(egui::Slider::new(&mut p.theta, 0.1..=1.0));
                ui.end_row();
                param(ui, tr("Clean-up: period (steps)"), tr("How many steps between removals of ejected bodies (0: never)."));
                ui.add(egui::DragValue::new(&mut p.cleanup_period).range(0..=1_000_000));
                ui.end_row();
                param(ui, tr("Clean-up: distance (× radius)"), tr("Only unbound bodies beyond this distance from the barycentre (in cloud radii) are removed; bound ejected clusters are kept."));
                ui.add(
                    egui::DragValue::new(&mut p.cleanup_distance_factor)
                        .range(1.0..=1e6)
                        .speed(0.1),
                );
                ui.end_row();
                param(ui, tr("Ejected clusters: at least (bodies)"), tr("A bound ejected cluster with at least this many bodies is kept by the clean-up (0: criterion ignored)."));
                ui.add(egui::DragValue::new(&mut p.cluster_min_bodies).range(0..=10_000_000));
                ui.end_row();
                param(ui, tr("… or at least (mass)"), tr("… or with at least this share of the initial mass, even if it has a single body (0: criterion ignored)."));
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
                param(ui, tr("Computation engine"), tr("GPU: graphics card, single precision, much faster. CPU: all cores but one, double precision."));
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

    /// In automatic mode, dt, ε, r₀ and the cluster link length follow the
    /// initial conditions; θ, the cleanup and the cluster thresholds keep the
    /// values typed in.
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
            ui.heading(tr("Saved simulations"));
            if ui.button("⟳").on_hover_text(tr("Refresh")).clicked() {
                self.refresh(service);
            }
            if ui.button(tr("Browse…")).clicked() {
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
            ui.label(RichText::new(tr("No save in the “saves” folder. Press F5 during a simulation.")).weak());
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
                                    .format(tr("%Y-%m-%d %H:%M"))
                                    .to_string()
                            })
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(tr!("{} · {} bodies · step {} · t = {}", date, format::count(s.body_count as u64), format::count(s.step), format::real(s.time)))
                            .weak(),
                        );
                        if let Some(rec) = &s.recording {
                            ui.label(
                                RichText::new(tr!("Resumes the recording “{}”", rec))
                                    .small()
                                    .color(egui::Color32::from_rgb(255, 120, 120)),
                            );
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if ui.button(tr("Load")).clicked() {
                        action = Some(HomeAction::Load(s.path.clone()));
                    }
                    if ui
                        .button(tr("Continue a recording…"))
                        .on_hover_text(tr("Load this save and extend an existing recording from its step"))
                        .clicked()
                    {
                        to_continue = Some(s.clone());
                    }
                    if ui.small_button(tr("Rename…")).clicked() {
                        file_op = Some((false, FileTarget::Save(s.clone())));
                    }
                    if ui.small_button(tr("Delete…")).clicked() {
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
            ui.heading(tr("Recordings"));
            if ui.button(tr("Browse…")).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Enregistrement Stellar", &["stellarec"])
                    .pick_file()
                {
                    action = Some(HomeAction::Replay(path));
                }
            }
        });
        if self.recordings.is_empty() {
            ui.label(RichText::new(tr("No recording in the “recordings” folder. Press F9 during a simulation.")).weak());
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
                            .map(|d| d.with_timezone(&chrono::Local).format(tr("%Y-%m-%d %H:%M")).to_string())
                            .unwrap_or_default();
                        let size = if r.bytes >= 1_000_000_000 {
                            tr!("{} GB", format!("{:.2}", r.bytes as f64 / 1e9))
                        } else {
                            tr!("{} MB", format!("{:.0}", r.bytes as f64 / 1e6))
                        }
                        .replace('.', crate::format::decimal_sep());
                        ui.label(
                            RichText::new(tr!("{} · {} snapshots · {} bodies at the start · duration {} · {}{}", date, format::count(r.frame_count as u64), format::count(r.first_count as u64), format::real(r.duration), size, if r.complete { "" } else { tr(" · unfinished (recovered)") }))
                            .weak(),
                        );
                    });
                });
                ui.horizontal(|ui| {
                    if ui.button(tr("Play")).clicked() {
                        action = Some(HomeAction::Replay(r.path.clone()));
                    }
                    // Most recent resume save linked to this recording.
                    let file = r.path.file_name().map(|n| n.to_string_lossy().into_owned());
                    if let Some(save) = self.saves.iter().find(|s| s.recording.is_some() && s.recording == file) {
                        if ui
                            .button(tr("Resume"))
                            .on_hover_text(tr!("Reloads the save “{}” (step {}) and continues the recording", save.name, format::count(save.step)))
                            .clicked()
                        {
                            action = Some(HomeAction::Load(save.path.clone()));
                        }
                    }
                    if ui.small_button(tr("Rename…")).clicked() {
                        file_op = Some((false, FileTarget::Recording(r.clone())));
                    }
                    if ui.small_button(tr("Delete…")).clicked() {
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

/// Bytes of main memory per body (measured: ≈ 4.2 GB for 10 million bodies,
/// engine, snapshots and rendering included).
const RAM_PER_BODY: f64 = 420.0;
/// Cap of the offered body count.
const MAX_COUNT: usize = 1_000_000_000;

/// Total main memory of the machine (bytes), if known.
#[cfg(windows)]
fn total_memory() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: initialized structure, with its size filled in.
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys)
}

#[cfg(not(windows))]
fn total_memory() -> Option<u64> {
    None
}

/// Maximum body count offered: engine limit, 80 % of main memory, one
/// billion at most.
fn max_bodies(service: &SimulatorService, engine: &str) -> usize {
    let by_ram = total_memory().map_or(usize::MAX, |m| (m as f64 * 0.8 / RAM_PER_BODY) as usize);
    service
        .max_bodies(engine)
        .unwrap_or(usize::MAX)
        .min(by_ram)
        .clamp(1_000, MAX_COUNT)
}

/// 1-2-5 steps from 1,000 to `max` (included, added if it is not a step).
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

/// Logarithmic slider (1-2-5 steps) and exact input of the body count.
fn count_picker(ui: &mut egui::Ui, count: &mut usize, max: usize) {
    *count = (*count).clamp(2, max);
    let steps = count_steps(max);
    // Nearest step at or below the value.
    let mut index = steps.iter().rposition(|&s| s <= *count).unwrap_or(0);
    ui.horizontal(|ui| {
        let slider = egui::Slider::new(&mut index, 0..=steps.len() - 1).show_value(false);
        if ui
            .add(slider)
            .on_hover_text(tr!("Maximum on this machine: {}", format::count(max as u64)))
            .changed()
        {
            *count = steps[index];
        }
        let speed = (*count as f64 * 0.01).max(1.0);
        ui.add(egui::DragValue::new(count).range(2..=max).speed(speed))
        .on_hover_text(tr("Exact value"));
    });
}

/// "?" badge explaining a parameter: tooltip on hover, and a bubble that
/// stays open on click (egui hides a tooltip after a click until the mouse
/// moves, and a "?" invites a click).
fn help(ui: &mut egui::Ui, text: &str) {
    let r = ui
        .add(egui::Label::new(RichText::new("?").small().strong().weak()).sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::Help);
    let id = r.id.with("help");
    if r.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    let open = ui.memory(|m| m.is_popup_open(id));
    egui::popup_below_widget(ui, id, &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_max_width(360.0);
        ui.label(text);
    });
    if !open {
        r.on_hover_text(text);
    }
}

/// Label of a parameter followed by its help badge.
fn param(ui: &mut egui::Ui, label: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        help(ui, text);
    });
}

/// Estimated duration of a step (seconds), from the benchmark: GPU, 2.2 ms
/// at 100,000 bodies, 14 ms at 1 M, 62 ms at 4 M; CPU, 232 ms at 1 M; in
/// n·log n.
pub fn step_estimate(count: usize, gpu: bool) -> f64 {
    let n = count.max(2) as f64;
    if gpu {
        0.9e-3 + 13.5e-3 * (n / 1e6).powf(1.055)
    } else {
        0.2e-3 + 0.232 * (n / 1e6) * n.log2() / 1e6f64.log2()
    }
}

/// Readable duration: milliseconds, seconds or minutes.
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

/// Abbreviated number: 1k, 2,5M (2.5M in English), 1G…
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

/// Share as a percentage, without sign (99,9 %).
fn share_text(share: f64) -> String {
    let text = format!("{:.3}", 100.0 * share);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{} %", text.replace('.', crate::format::decimal_sep()))
}

/// Input of a share of the total mass, as a percentage.
fn share_input(ui: &mut egui::Ui, share: &mut f64, min: f64, max: f64, suffix: &str) {
    let mut pct = *share * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut pct)
                .range(min * 100.0..=max * 100.0)
                .speed(0.1)
                .max_decimals(3)
                .suffix(suffix),
        )
        .changed()
    {
        *share = pct / 100.0;
    }
}

/// Default name of a simulation: date, abbreviated particle count,
/// distribution and initial velocity (`2026-10-28_1M_sph_orb`); the time is
/// added if `taken` says the name already exists.
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

    /// Every "?" badge of the form explains its parameter on hover, and keeps
    /// the explanation open after a click (a click hides egui's tooltips).
    #[test]
    fn help_badges_explain_on_hover_and_on_click() {
        use stellar_persistence::{FileRecordingRepository, FileRepository};
        let dir = std::env::temp_dir().join("stellar-test-help-badges");
        let service = SimulatorService::new(
            stellar_application::EngineRegistry::new(),
            std::sync::Arc::new(FileRepository::new(dir.join("saves"))),
            std::sync::Arc::new(FileRecordingRepository::new(dir.join("recordings"))),
        );
        let mut home = HomeScreen::new(&service);
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut frame = |events: Vec<egui::Event>| {
            time += 0.02;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                home.show(ctx, &service);
            });
            out.shapes
                .into_iter()
                .filter_map(|s| match s.shape {
                    egui::epaint::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.rect.center().to_vec2())),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let texts = frame(Vec::new());
        let base: std::collections::HashSet<String> = texts.iter().map(|t| t.0.clone()).collect();
        let marks: Vec<egui::Pos2> = texts.iter().filter(|t| t.0 == "?").map(|t| t.1).collect();
        assert!(marks.len() >= 10, "{} badges", marks.len());
        let click = |at, pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        // Explanation shown at the end of a few frames (`events`: those of the
        // first frames).
        let mut shown_after = |events: Vec<Vec<egui::Event>>| {
            let mut last = Vec::new();
            for e in events {
                last = frame(e);
            }
            for _ in 0..50 {
                last = frame(Vec::new());
            }
            last.into_iter().any(|t| !base.contains(&t.0) && t.0.len() > 20)
        };
        for &at in &marks {
            // Gradual approach, like a real mouse: egui only sees a movement
            // through the pointer's velocity.
            let hover = (0..=5)
                .rev()
                .map(|k| vec![egui::Event::PointerMoved(at + egui::vec2(6.0 * k as f32, 0.0))])
                .collect();
            assert!(shown_after(hover), "no tooltip on hover at {at:?}");
            assert!(shown_after(vec![vec![click(at, true), click(at, false)]]), "nothing shown after a click at {at:?}");
            // A click elsewhere (the footer text) closes it.
            let away = egui::pos2(20.0, 890.0);
            assert!(!shown_after(vec![vec![egui::Event::PointerMoved(away), click(away, true), click(away, false)]]));
            shown_after(vec![vec![egui::Event::PointerGone]]);
        }
    }

    #[test]
    fn default_names() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 28, 14, 5, 0).unwrap();
        let ic = InitialConditions {
            count: 1_000_000,
            velocity: VelocityMode::CircularOrbital { spread: 0.05, factor: 1.0 },
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

