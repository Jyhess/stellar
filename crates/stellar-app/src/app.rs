//! Application egui : enchaînement des écrans.

use std::time::{Duration, Instant};

use eframe::egui;
use stellar_application::SimulatorService;

use crate::home::{HomeAction, HomeScreen};
use crate::render::PointRenderer;
use crate::sim::{SimAction, SimScreen};

enum Screen {
    Home(HomeScreen),
    Sim(SimScreen),
    /// État transitoire pendant un changement d'écran.
    Switching,
}

/// Plafond de la cadence d'affichage.
pub const MAX_FPS: f64 = 120.0;

pub struct StellarApp {
    service: SimulatorService,
    screen: Screen,
    last_frame: Option<Instant>,
}

impl StellarApp {
    pub fn new(cc: &eframe::CreationContext<'_>, service: SimulatorService) -> Self {
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            PointRenderer::install(rs);
        }
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let home = HomeScreen::new(&service);
        let mut app = Self { service, screen: Screen::Home(home), last_frame: None };
        app.autostart();
        app
    }

    /// Démarrage direct, pour les essais : `STELLAR_AUTOSTART=<nombre de particules>`.
    fn autostart(&mut self) {
        let Some(count) = std::env::var("STELLAR_AUTOSTART").ok().and_then(|v| v.parse::<usize>().ok()) else {
            return;
        };
        let mut config = stellar_domain::SimulationConfig::default();
        config.initial.count = count.max(2);
        config.physics = stellar_domain::PhysicsParams::suggested(&config.initial, 1.0);
        if let Ok(handle) = self.service.new_simulation(config) {
            self.screen = Screen::Sim(SimScreen::new(handle, self.service.clone(), None));
        }
    }

    fn go_home(&mut self, ctx: &egui::Context, message: Option<String>) {
        if let Screen::Sim(sim) = std::mem::replace(&mut self.screen, Screen::Switching) {
            sim.shutdown(ctx);
        }
        let mut home = HomeScreen::new(&self.service);
        home.message = message;
        self.screen = Screen::Home(home);
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

        match &mut self.screen {
            Screen::Home(home) => match home.show(ctx, &self.service) {
                HomeAction::None => {}
                HomeAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                HomeAction::Start(config) => match self.service.new_simulation(config) {
                    Ok(handle) => {
                        self.screen = Screen::Sim(SimScreen::new(handle, self.service.clone(), None));
                    }
                    Err(e) => home.message = Some(e.to_string()),
                },
                HomeAction::Load(path) => match self.service.load_simulation(&path) {
                    Ok((handle, view)) => {
                        self.screen = Screen::Sim(SimScreen::new(handle, self.service.clone(), view));
                    }
                    Err(e) => home.message = Some(format!("Chargement impossible : {e}")),
                },
            },
            Screen::Sim(sim) => match sim.show(ctx) {
                SimAction::None => {}
                SimAction::BackHome => self.go_home(ctx, None),
                SimAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            },
            Screen::Switching => {}
        }
    }
}
