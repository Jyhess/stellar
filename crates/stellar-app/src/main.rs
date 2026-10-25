//! # Stellar
//!
//! Racine de composition : assemble les couches (domaine, cas d'usage,
//! moteur CPU, persistance) et lance l'interface graphique.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod camera;
mod format;
mod home;
mod picking;
mod render;
mod sim;

use std::sync::Arc;

use eframe::egui;
use stellar_application::{EngineRegistry, SimulatorService};
use stellar_engine_cpu::CpuEngineFactory;
use stellar_persistence::FileRepository;

/// Dossier des sauvegardes, relatif au répertoire de lancement.
const SAVES_DIR: &str = "saves";

fn main() -> eframe::Result {
    let mut registry = EngineRegistry::new();
    registry.register(Arc::new(CpuEngineFactory));
    // Un futur moteur (GPU…) s'enregistre ici et devient sélectionnable.

    let repository = Arc::new(FileRepository::new(SAVES_DIR));
    let service = SimulatorService::new(registry, repository);

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        vsync: true,
        viewport: egui::ViewportBuilder::default()
            .with_title("Stellar")
            .with_inner_size([1600.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Stellar",
        options,
        Box::new(move |cc| Ok(Box::new(app::StellarApp::new(cc, service)))),
    )
}
