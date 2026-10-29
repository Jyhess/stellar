//! # Stellar
//!
//! Racine de composition : assemble les couches (domaine, cas d'usage,
//! moteurs, persistance). Deux exécutables s'en servent :
//! - `stellar` (`main.rs`) : l'interface graphique ([`run_gui`]) ;
//! - `stellar-cli` (`cli.rs`) : la ligne de commande ([`console::run`]).

#[macro_use]
pub mod i18n;
mod app;
mod camera;
pub mod console;
mod format;
mod headless;
mod home;
mod picking;
mod render;
mod sim;
mod source;

use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use stellar_application::{EngineRegistry, SimulatorService};
use stellar_engine_cpu::CpuEngineFactory;
use stellar_engine_gpu::GpuEngineFactory;
use stellar_persistence::{FileRecordingRepository, FileRepository};

use crate::i18n::tr;

/// Dossier des sauvegardes, relatif au répertoire de lancement.
const SAVES_DIR: &str = "saves";
/// Dossier des enregistrements (relecture).
const RECORDINGS_DIR: &str = "recordings";

/// Journal des plantages, écrit dans le répertoire de lancement.
const CRASH_LOG: &str = "stellar-crash.log";

/// En mode fenêtré, Windows n'affiche pas la console : toute erreur fatale est
/// consignée dans `stellar-crash.log` et signalée par une boîte de dialogue.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("?").to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = tr!("[{}] Plantage dans le thread « {} »\n{}\n\n{}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), thread, info, backtrace);
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
            .set_title(tr("Stellar — erreur fatale"))
            .set_description(tr!("{}\n\nDétails enregistrés dans {}.", info, CRASH_LOG))
            .show();
        default(info);
    }));
}

/// Lance l'interface graphique.
pub fn run_gui() -> eframe::Result {
    install_panic_hook();
    i18n::init();
    let mut registry = EngineRegistry::new();
    // Le moteur GPU devient le moteur par défaut si une carte compatible est
    // présente ; le moteur CPU reste toujours disponible.
    let gpu = GpuEngineFactory::probe();
    // Le moteur GPU partagera la carte graphique de l'interface (affichage
    // des corps sans copie) dès que celle-ci sera ouverte.
    let gpu_factory = gpu.is_ok().then(|| Arc::new(GpuEngineFactory::new()));
    if let Some(f) = &gpu_factory {
        registry.register(f.clone());
    }
    registry.register(Arc::new(CpuEngineFactory));

    let repository = Arc::new(FileRepository::new(SAVES_DIR).with_trash());
    let recordings = Arc::new(FileRecordingRepository::new(RECORDINGS_DIR).with_trash());
    let mut service = SimulatorService::new(registry, repository, recordings);
    service.set_engine_note(match gpu {
        Ok(name) => tr!("Carte graphique : {}", name),
        Err(e) => tr!("Moteur GPU indisponible : {}", e),
    });

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        vsync: true,
        wgpu_options: wgpu_configuration(),
        viewport: egui::ViewportBuilder::default()
            .with_title("Stellar")
            .with_inner_size([1600.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Stellar",
        options,
        Box::new(move |cc| Ok(Box::new(app::StellarApp::new(cc, service, gpu_factory)))),
    )
}

/// Carte graphique de l'interface : la plus performante, avec toutes ses
/// limites (le moteur GPU, qui la partage, manipule des tampons de plusieurs
/// centaines de mégaoctets, bien au-delà des limites par défaut de wgpu).
fn wgpu_configuration() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    };
    let default_descriptor = setup.device_descriptor.clone();
    setup.device_descriptor = Arc::new(move |adapter: &wgpu::Adapter| {
        let mut descriptor = default_descriptor(adapter);
        if adapter.get_info().backend != wgpu::Backend::Gl {
            descriptor.label = Some("stellar-device");
            descriptor.required_limits = adapter.limits();
            descriptor.memory_hints = wgpu::MemoryHints::Performance;
        }
        descriptor
    });
    egui_wgpu::WgpuConfiguration {
        present_mode: present_mode(),
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// Mode de présentation de l'interface.
///
/// En mode synchronisé classique (Fifo), l'acquisition de l'image suivante
/// attend le balayage de l'écran en tenant un verrou de la carte que toute
/// soumission de travaux réclame aussi : le moteur GPU, qui partage la carte,
/// perdrait jusqu'à une image entière à chaque pas. Le mode Mailbox reste
/// synchronisé (pas de déchirement) sans cette attente ; il est garanti sous
/// DirectX 12 et proposé par les pilotes Vulkan de NVIDIA et d'AMD.
///
/// `STELLAR_PRESENT=fifo|mailbox|immediate` impose un mode (essais).
fn present_mode() -> wgpu::PresentMode {
    match std::env::var("STELLAR_PRESENT").as_deref() {
        Ok("fifo") => return wgpu::PresentMode::AutoVsync,
        Ok("mailbox") => return wgpu::PresentMode::Mailbox,
        Ok("immediate") => return wgpu::PresentMode::AutoNoVsync,
        _ => {}
    }
    const NVIDIA: u32 = 0x10de;
    const AMD: u32 = 0x1002;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }));
    match adapter.map(|a| a.get_info()) {
        Some(info)
            if info.backend == wgpu::Backend::Dx12
                || (info.backend == wgpu::Backend::Vulkan
                    && matches!(info.vendor, NVIDIA | AMD)) =>
        {
            wgpu::PresentMode::Mailbox
        }
        _ => wgpu::PresentMode::AutoVsync,
    }
}
