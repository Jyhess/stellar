//! # Stellar
//!
//! Composition root: assembles the layers (domain, use cases, engines,
//! persistence). Two executables use it:
//! - `stellar` (`main.rs`): the graphical interface ([`run_gui`]);
//! - `stellar-cli` (`cli.rs`): the command line ([`console::run`]).

#[macro_use]
pub mod i18n;
mod app;
mod camera;
mod errors;
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

use crate::errors::Localized;
use crate::i18n::tr;

/// Folder of the saves, relative to the current directory.
const SAVES_DIR: &str = "saves";
/// Folder of the recordings (playback).
const RECORDINGS_DIR: &str = "recordings";

/// Crash log, written in the current directory.
const CRASH_LOG: &str = "stellar-crash.log";

/// In windowed mode, Windows shows no console: any fatal error is written to
/// `stellar-crash.log` and reported in a dialog box.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("?").to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = tr!("[{}] Crash in thread “{}”\n{}\n\n{}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), thread, info, backtrace);
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
            .set_title(tr("Stellar — fatal error"))
            .set_description(tr!("{}\n\nDetails written to {}.", info, CRASH_LOG))
            .show();
        default(info);
    }));
}

/// Starts the graphical interface.
pub fn run_gui() -> eframe::Result {
    install_panic_hook();
    i18n::init();
    let mut registry = EngineRegistry::new();
    // The GPU engine becomes the default engine if a compatible card is
    // present; the CPU engine always remains available.
    let gpu = GpuEngineFactory::probe();
    // The GPU engine will share the interface's device (drawing the bodies
    // without copy) as soon as it is opened.
    let gpu_factory = gpu.is_ok().then(|| Arc::new(GpuEngineFactory::new()));
    if let Some(f) = &gpu_factory {
        registry.register(f.clone());
    }
    registry.register(Arc::new(CpuEngineFactory));

    let repository = Arc::new(FileRepository::new(SAVES_DIR).with_trash());
    let recordings = Arc::new(FileRecordingRepository::new(RECORDINGS_DIR).with_trash());
    let mut service = SimulatorService::new(registry, repository, recordings);
    service.set_engine_note(match gpu {
        Ok(name) => tr!("Graphics card: {}", name),
        Err(e) => tr!("GPU engine unavailable: {}", e.localized()),
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

/// Graphics device of the interface: the most powerful one, with all its
/// limits (the GPU engine, which shares it, handles buffers of several
/// hundred megabytes, well beyond wgpu's default limits).
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

/// Presentation mode of the interface.
///
/// In classic synchronized mode (Fifo), acquiring the next frame waits for
/// the screen refresh while holding a device lock that every submission
/// also needs: the GPU engine, which shares the device, would lose up to a
/// whole frame at every step. Mailbox mode stays synchronized (no tearing)
/// without that wait; it is guaranteed under DirectX 12 and offered by the
/// NVIDIA and AMD Vulkan drivers.
///
/// `STELLAR_PRESENT=fifo|mailbox|immediate` forces a mode (testing).
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
