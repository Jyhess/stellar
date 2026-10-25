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
mod source;

use std::sync::Arc;

use eframe::egui;
use stellar_application::{EngineRegistry, SimulatorService};
use stellar_engine_cpu::CpuEngineFactory;
use stellar_persistence::{FileRecordingRepository, FileRepository};

/// Dossier des sauvegardes, relatif au répertoire de lancement.
const SAVES_DIR: &str = "saves";
/// Dossier des enregistrements (relecture).
const RECORDINGS_DIR: &str = "recordings";

/// Journal des plantages, écrit dans le répertoire de lancement.
const CRASH_LOG: &str = "stellar-crash.log";

/// En mode fenêtré, Windows n'affiche pas la console : toute erreur fatale est
/// consignée dans `stellar-crash.log` et signalée par une boîte de dialogue.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("?").to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!(
            "[{}] Plantage dans le thread « {thread} »\n{info}\n\n{backtrace}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        );
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(CRASH_LOG)
        {
            let _ = f.write_all(report.as_bytes());
        }
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Stellar — erreur fatale")
            .set_description(format!("{info}\n\nDétails enregistrés dans {CRASH_LOG}."))
            .show();
        default(info);
    }));
}

fn main() -> eframe::Result {
    install_panic_hook();
    let mut registry = EngineRegistry::new();
    registry.register(Arc::new(CpuEngineFactory));
    // Un futur moteur (GPU…) s'enregistre ici et devient sélectionnable.

    let repository = Arc::new(FileRepository::new(SAVES_DIR));
    let recordings = Arc::new(FileRecordingRepository::new(RECORDINGS_DIR));
    let service = SimulatorService::new(registry, repository, recordings);

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
