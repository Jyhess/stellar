//! Application egui : enchaînement des écrans.

use std::time::{Duration, Instant};

use std::sync::Arc;

use eframe::egui;
use stellar_application::SimulatorService;
use stellar_engine_gpu::{GpuEngineFactory, SharedDevice};

use crate::headless::{HeadlessAction, HeadlessScreen};
use crate::home::{HomeAction, HomeScreen};
use crate::render::PointRenderer;
use crate::sim::{SimAction, SimScreen};
use crate::source::Source;

enum Screen {
    // En boîte : les deux écrans sont volumineux et de tailles très
    // différentes.
    Home(Box<HomeScreen>),
    Sim(Box<SimScreen>),
    /// Simulation sans affichage.
    Headless(Box<HeadlessScreen>),
    /// État transitoire pendant un changement d'écran.
    Switching,
}

impl Screen {
    fn sim(screen: SimScreen) -> Self {
        Self::Sim(Box::new(screen))
    }
}

/// Plafond de la cadence d'affichage.
pub const MAX_FPS: f64 = 120.0;

pub struct StellarApp {
    service: SimulatorService,
    screen: Screen,
    last_frame: Option<Instant>,
}

impl StellarApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        service: SimulatorService,
        gpu_factory: Option<Arc<GpuEngineFactory>>,
    ) -> Self {
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            PointRenderer::install(rs);
            // Le moteur GPU calcule sur la carte de l'interface : ses corps
            // sont dessinés directement depuis ses tampons.
            if let Some(f) = &gpu_factory {
                f.attach(SharedDevice {
                    adapter: rs.adapter.clone(),
                    device: rs.device.clone(),
                    queue: rs.queue.clone(),
                });
            }
        }
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let home = HomeScreen::new(&service);
        let mut app = Self {
            service,
            screen: Screen::Home(Box::new(home)),
            last_frame: None,
        };
        app.autostart();
        app
    }

    /// Démarrage direct, pour les essais : `STELLAR_AUTOSTART=<nombre de particules>`.
    fn autostart(&mut self) {
        // `STELLAR_LOAD=<sauvegarde>` : chargement direct (reprise d'enregistrement comprise).
        if let Ok(path) = std::env::var("STELLAR_LOAD") {
            if let Ok(loaded) = self.service.load_simulation(std::path::Path::new(&path)) {
                self.screen = Screen::sim(SimScreen::new(
                    Source::Live(Box::new(loaded.handle)),
                    self.service.clone(),
                    loaded.view,
                ));
                return;
            }
        }
        // `STELLAR_REPLAY=<fichier>` : relecture directe.
        if let Ok(path) = std::env::var("STELLAR_REPLAY") {
            if let Ok(player) = self.service.open_replay(std::path::Path::new(&path)) {
                let source = Source::Replay(Box::new(crate::source::Replay::new(player)));
                self.screen = Screen::sim(SimScreen::new(source, self.service.clone(), None));
                return;
            }
        }
        let Some(count) = std::env::var("STELLAR_AUTOSTART")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        else {
            return;
        };
        let mut config = stellar_domain::SimulationConfig::default();
        // `STELLAR_ENGINE=<id>` : moteur imposé (sinon le moteur par défaut).
        config.engine = std::env::var("STELLAR_ENGINE")
            .ok()
            .or_else(|| self.service.default_engine().map(str::to_owned))
            .unwrap_or(config.engine);
        config.initial.count = count.max(2);
        // `STELLAR_AUTODISK=1` : disque incliné en rotation orbitale (essais).
        if std::env::var("STELLAR_AUTODISK").is_ok() {
            config.initial.distribution = stellar_domain::Distribution::Disk {
                thickness: 0.05,
                profile: stellar_domain::DensityProfile::Exponential { scale: 0.3 },
            };
            config.initial.velocity =
                stellar_domain::VelocityMode::CircularOrbital { spread: 0.05, factor: 1.0 };
        }
        config.physics = stellar_domain::PhysicsParams::suggested(&config.initial, 1.0);
        let name = crate::home::default_name(&config.initial, chrono::Local::now(), |_| false);
        if let Ok(handle) = self.service.new_simulation(config) {
            handle.set_name(&name);
            // `STELLAR_AUTORECORD=<intervalle>` : enregistrement immédiat.
            if let Some(interval) = std::env::var("STELLAR_AUTORECORD")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
            {
                let _ =
                    self.service
                        .start_recording(&name, &handle, interval, Some(1_000_000));
            }
            self.screen = Screen::sim(SimScreen::new(
                Source::Live(Box::new(handle)),
                self.service.clone(),
                None,
            ));
        }
    }

    /// Si un enregistrement est en cours, le suspend et sauvegarde la
    /// simulation (liée à l'enregistrement) pour une reprise ultérieure.
    fn suspend_recording(&mut self) -> Option<String> {
        let Screen::Sim(sim) = &self.screen else {
            return None;
        };
        sim.suspend_recording().map(|r| match r {
            Ok(path) => tr!("Enregistrement suspendu. Il reprendra en rechargeant la sauvegarde « {} ».", path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()),
            Err(e) => tr!("La suspension de l'enregistrement a échoué : {}", e),
        })
    }

    fn go_home(&mut self, ctx: &egui::Context, message: Option<String>) {
        let message = self.suspend_recording().or(message);
        if let Screen::Sim(sim) = std::mem::replace(&mut self.screen, Screen::Switching) {
            sim.shutdown(ctx);
        }
        let mut home = HomeScreen::new(&self.service);
        home.message = message;
        self.screen = Screen::Home(Box::new(home));
    }
}

impl eframe::App for StellarApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Limiteur de cadence : aucune image avant 1/MAX_FPS s, y compris
        // lorsque les mouvements de souris déclenchent des repeints.
        let min_interval = Duration::from_secs_f64(1.0 / MAX_FPS);
        if let Some(t) = self.last_frame {
            let elapsed = t.elapsed();
            if elapsed < min_interval {
                std::thread::sleep(min_interval - elapsed);
            }
        }
        self.last_frame = Some(Instant::now());

        // Fermeture de la fenêtre pendant un enregistrement : suspension et
        // sauvegarde de reprise avant de quitter.
        if ctx.input(|i| i.viewport().close_requested()) {
            if let Screen::Headless(h) = &mut self.screen {
                if h.is_running() {
                    h.finish_now();
                }
            }
            if let Screen::Sim(sim) = &self.screen {
                if sim.is_recording() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    let _ = self.suspend_recording();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            }
        }

        match &mut self.screen {
            Screen::Home(home) => match home.show(ctx, &self.service) {
                HomeAction::None => {}
                HomeAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                HomeAction::StartHeadless(config, record, name, target) => {
                    match stellar_application::HeadlessRun::start(&self.service, config, &name, record, target) {
                        Ok(run) => {
                            self.screen = Screen::Headless(Box::new(HeadlessScreen::new(
                                self.service.clone(),
                                run,
                            )));
                        }
                        Err(e) => home.message = Some(e.to_string()),
                    }
                }
                // En pause, pour armer l'enregistrement avant le premier pas.
                HomeAction::Start(config, record, name) => match self.service.new_simulation_paused(config) {
                    Ok(handle) => {
                        handle.set_name(&name);
                        if let Some(r) = record {
                            if let Err(e) = self.service.start_recording(
                                &name,
                                &handle,
                                r.interval,
                                r.dust_budget,
                            ) {
                                home.message = Some(tr!("Enregistrement impossible : {}", e));
                            }
                        }
                        handle.resume();
                        self.screen = Screen::sim(SimScreen::new(
                            Source::Live(Box::new(handle)),
                            self.service.clone(),
                            None,
                        ));
                    }
                    Err(e) => home.message = Some(e.to_string()),
                },
                a @ (HomeAction::Load(_) | HomeAction::LoadContinuing(..)) => match match a {
                    HomeAction::LoadContinuing(data, path, save_name) => {
                        self.service.start_loaded(*data, Some(&path)).inspect(|l| {
                            l.handle.set_name(&stellar_application::name_from_file(&save_name))
                        })
                    }
                    HomeAction::Load(path) => self.service.load_simulation(&path),
                    _ => unreachable!(),
                } {
                    Ok(loaded) => {
                        let mut sim = SimScreen::new(
                            Source::Live(Box::new(loaded.handle)),
                            self.service.clone(),
                            loaded.view,
                        );
                        match loaded.recording {
                            Some(Ok(p)) => sim.notify(tr!("Enregistrement « {} » poursuivi : il reprend dès la relance du calcul (Espace).", p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())),
                            Some(Err(e)) => sim.notify(tr!("Reprise de l'enregistrement impossible : {}", e)),
                            None => {}
                        }
                        self.screen = Screen::sim(sim);
                    }
                    Err(e) => home.message = Some(tr!("Chargement impossible : {}", e)),
                },
                HomeAction::Replay(path) => match self.service.open_replay(&path) {
                    Ok(player) => {
                        let source = Source::Replay(Box::new(crate::source::Replay::new(player)));
                        self.screen =
                            Screen::sim(SimScreen::new(source, self.service.clone(), None));
                    }
                    Err(e) => home.message = Some(tr!("Relecture impossible : {}", e)),
                },
            },
            Screen::Sim(sim) => match sim.show(ctx) {
                SimAction::None => {}
                SimAction::BackHome => self.go_home(ctx, None),
                SimAction::Quit => {
                    let _ = self.suspend_recording();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            },
            Screen::Headless(h) => match h.show(ctx) {
                HeadlessAction::None => {}
                HeadlessAction::BackHome => self.go_home(ctx, None),
                HeadlessAction::Show(run) => {
                    self.screen = Screen::sim(SimScreen::new(
                        Source::Live(Box::new(run.into_handle())),
                        self.service.clone(),
                        None,
                    ));
                }
            },
            Screen::Switching => {}
        }
    }
}
