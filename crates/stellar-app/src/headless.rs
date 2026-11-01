//! Monitoring screen of a simulation without display: no rendering of the
//! bodies, the whole graphics card for the computation; recording is automatic.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{self, RichText};
use stellar_application::{HeadlessRun, RunOutcome, RunTarget, SimulatorService, ViewState};

use crate::format;
use crate::errors::Localized;
use crate::i18n::tr;

pub enum HeadlessAction {
    None,
    /// Show the simulation (computation and recording continue), with the
    /// camera it had when sent to the background.
    Show(Box<HeadlessRun>, Option<ViewState>),
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
    /// Camera of a simulation sent to the background from the display.
    view: Option<ViewState>,
}

/// Readable duration: 1 h 02 min 03 s.
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
    pub fn new(service: SimulatorService, run: HeadlessRun, view: Option<ViewState>) -> Self {
        Self {
            service,
            name: run.handle().name(),
            state: Some(State::Running(Box::new(run))),
            view,
        }
    }

    /// Immediate stop (window closed): closing and saving, waiting for them to
    /// finish.
    pub fn finish_now(&mut self) {
        self.finish();
        if let Some(State::Finishing(rx)) = &self.state {
            let _ = rx.recv();
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, Some(State::Running(_)) | Some(State::Finishing(_)))
    }

    /// Stop: closing and saving in the background.
    fn finish(&mut self) {
        if let Some(State::Running(run)) = self.state.take() {
            let (tx, rx) = mpsc::channel();
            let service = self.service.clone();
            std::thread::spawn(move || {
                let _ = tx.send(run.finish(&service).map_err(|e| e.localized()));
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
                ui.heading(RichText::new(tr("Simulation without display")).size(28.0));
                ui.label(RichText::new(&self.name).strong());
                ui.add_space(16.0);
            });
            match &mut self.state {
                Some(State::Running(run)) => {
                    let p = run.poll();
                    let target = run.target();
                    if let Some(e) = &p.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, e.localized());
                    }
                    if let Some(f) = p.fraction {
                        let eta = (f > 0.0 && f < 1.0)
                            .then(|| p.elapsed.mul_f64((1.0 - f) / f));
                        let pct = format!("{:.1}", 100.0 * f).replace('.', format::decimal_sep());
                        let text = match eta {
                            Some(eta) => tr!("{}% · ≈ {} left", pct, hms(eta)),
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
                                tr("State"),
                                if p.ready { tr("running") } else { tr("preparing…") }.to_owned(),
                            );
                            row(
                                tr("Target"),
                                match target {
                                    RunTarget::Manual => tr("manual stop").to_owned(),
                                    RunTarget::Steps(n) => tr!("{} steps", format::count(n)),
                                    RunTarget::SimTime(t) => tr!("simulated time {}", format::real(t)),
                                    RunTarget::WallTime(d) => tr!("duration {}", hms(d)),
                                },
                            );
                            row(tr("Step"), format::count(p.step));
                            row(tr("Simulated time"), format::real(p.time));
                            row(tr("Bodies"), format::count(p.bodies as u64));
                            row(
                                tr("Steps / s"),
                                format!("{:.1}", p.steps_per_second).replace('.', format::decimal_sep()),
                            );
                            row(tr("Elapsed time"), hms(p.elapsed));
                            row(
                                tr("Recording"),
                                if run.handle().is_recording() || p.frames > 0 {
                                    tr!(
                                        "{} snapshots · {}",
                                        format::count(p.frames),
                                        crate::sim::format_bytes(p.bytes)
                                    )
                                } else {
                                    tr("none").to_owned()
                                },
                            );
                        });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        stop = ui.button(tr("■ Stop and save")).clicked();
                        show = ui
                            .button(tr("Show the simulation"))
                            .on_hover_text(tr("The computation and the recording go on; the target is no longer watched."))
                            .clicked();
                    });
                    ui.label(
                        RichText::new(tr("When stopped, the recording is closed and the final state saved under the simulation's name."))
                            .weak(),
                    );
                }
                Some(State::Finishing(_)) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Closing the recording and saving…"));
                    });
                }
                Some(State::Done(r)) => {
                    match r {
                        Ok(o) => {
                            ui.label(tr!("Finished at step {}.", format::count(o.step)));
                            ui.label(tr!("Save: {}", o.save.display()));
                            if let Some(rec) = &o.recording {
                                ui.label(tr!("Recording: {}", rec.display()));
                            }
                        }
                        Err(e) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, tr!("Failure: {}", e));
                        }
                    }
                    ui.add_space(8.0);
                    if ui.button(tr("Back to the home screen")).clicked() {
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
                action = HeadlessAction::Show(run, self.view.take());
            }
        }
        // Moderate refresh: nothing else to draw.
        ctx.request_repaint_after(Duration::from_millis(250));
        action
    }
}
