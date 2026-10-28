//! Écran de suivi d'une simulation sans affichage : aucun rendu des corps,
//! toute la carte graphique pour le calcul ; l'enregistrement est automatique.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{self, RichText};
use stellar_application::{HeadlessRun, RunOutcome, RunTarget, SimulatorService};

use crate::format;
use crate::i18n::tr;

pub enum HeadlessAction {
    None,
    /// Afficher la simulation (le calcul et l'enregistrement continuent).
    Show(Box<HeadlessRun>),
    BackHome,
}

enum State {
    Running(Box<HeadlessRun>),
    Finishing(Receiver<Result<RunOutcome, String>>),
    Done(Result<RunOutcome, String>),
}

pub struct HeadlessScreen {
    service: SimulatorService,
    name: String,
    state: Option<State>,
}

/// Durée lisible : 1 h 02 min 03 s.
pub fn hms(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h} h {m:02} min {s:02} s")
    } else if m > 0 {
        format!("{m} min {s:02} s")
    } else {
        format!("{s} s")
    }
}

impl HeadlessScreen {
    pub fn new(service: SimulatorService, run: HeadlessRun) -> Self {
        Self {
            service,
            name: run.handle().name(),
            state: Some(State::Running(Box::new(run))),
        }
    }

    /// Arrêt immédiat (fermeture de la fenêtre) : clôture et sauvegarde,
    /// en attendant leur fin.
    pub fn finish_now(&mut self) {
        self.finish();
        if let Some(State::Finishing(rx)) = &self.state {
            let _ = rx.recv();
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, Some(State::Running(_)) | Some(State::Finishing(_)))
    }

    /// Arrêt : clôture et sauvegarde en arrière-plan.
    fn finish(&mut self) {
        if let Some(State::Running(run)) = self.state.take() {
            let (tx, rx) = mpsc::channel();
            let service = self.service.clone();
            std::thread::spawn(move || {
                let _ = tx.send(run.finish(&service).map_err(|e| e.to_string()));
            });
            self.state = Some(State::Finishing(rx));
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> HeadlessAction {
        let mut action = HeadlessAction::None;
        if let Some(State::Running(run)) = &mut self.state {
            if run.done() {
                self.finish();
            }
        }
        if let Some(State::Finishing(rx)) = &self.state {
            if let Ok(r) = rx.try_recv() {
                self.state = Some(State::Done(r));
            }
        }
        let mut stop = false;
        let mut show = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.heading(RichText::new(tr("Simulation sans affichage")).size(28.0));
                ui.label(RichText::new(&self.name).strong());
                ui.add_space(16.0);
            });
            match &mut self.state {
                Some(State::Running(run)) => {
                    let p = run.poll();
                    let target = run.target();
                    if let Some(e) = p.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, e);
                    }
                    if let Some(f) = p.fraction {
                        let eta = (f > 0.0 && f < 1.0)
                            .then(|| p.elapsed.mul_f64((1.0 - f) / f));
                        let pct = format!("{:.1}", 100.0 * f).replace('.', format::decimal_sep());
                        let text = match eta {
                            Some(eta) => tr!("{} % · reste ≈ {}", pct, hms(eta)),
                            None => format!("{pct} %"),
                        };
                        ui.add(egui::ProgressBar::new(f as f32).text(text));
                    }
                    egui::Grid::new("headless")
                        .num_columns(2)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            let mut row = |k: &str, v: String| {
                                ui.label(k);
                                ui.label(RichText::new(v).monospace());
                                ui.end_row();
                            };
                            row(
                                tr("État"),
                                if p.ready { tr("en cours") } else { tr("préparation…") }.to_owned(),
                            );
                            row(
                                tr("Cible"),
                                match target {
                                    RunTarget::Manual => tr("arrêt manuel").to_owned(),
                                    RunTarget::Steps(n) => tr!("{} pas", format::count(n)),
                                    RunTarget::SimTime(t) => tr!("temps simulé {}", format::real(t)),
                                    RunTarget::WallTime(d) => tr!("durée {}", hms(d)),
                                },
                            );
                            row(tr("Pas"), format::count(p.step));
                            row(tr("Temps simulé"), format::real(p.time));
                            row(tr("Corps"), format::count(p.bodies as u64));
                            row(
                                tr("Pas / s"),
                                format!("{:.1}", p.steps_per_second).replace('.', format::decimal_sep()),
                            );
                            row(tr("Durée écoulée"), hms(p.elapsed));
                            row(
                                tr("Enregistrement"),
                                tr!(
                                    "{} instantanés · {}",
                                    format::count(p.frames),
                                    crate::sim::format_bytes(p.bytes)
                                ),
                            );
                        });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        stop = ui.button(tr("■ Arrêter et sauvegarder")).clicked();
                        show = ui
                            .button(tr("Afficher la simulation"))
                            .on_hover_text(tr("Le calcul et l'enregistrement continuent ; la cible n'est plus surveillée."))
                            .clicked();
                    });
                    ui.label(
                        RichText::new(tr("À l'arrêt, l'enregistrement est clos et l'état final sauvegardé sous le nom de la simulation."))
                            .weak(),
                    );
                }
                Some(State::Finishing(_)) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Clôture de l'enregistrement et sauvegarde…"));
                    });
                }
                Some(State::Done(r)) => {
                    match r {
                        Ok(o) => {
                            ui.label(tr!("Terminé au pas {}.", format::count(o.step)));
                            ui.label(tr!("Sauvegarde : {}", o.save.display()));
                            if let Some(rec) = &o.recording {
                                ui.label(tr!("Enregistrement : {}", rec.display()));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, tr!("Échec : {}", e));
                        }
                    }
                    ui.add_space(8.0);
                    if ui.button(tr("Retour à l'accueil")).clicked() {
                        action = HeadlessAction::BackHome;
                    }
                }
                None => {}
            }
        });
        if stop {
            self.finish();
        } else if show {
            if let Some(State::Running(run)) = self.state.take() {
                action = HeadlessAction::Show(run);
            }
        }
        // Rafraîchissement modéré : rien à dessiner d'autre.
        ctx.request_repaint_after(Duration::from_millis(250));
        action
    }
}
