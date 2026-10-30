//! Stellar's graphical interface. The command line is served by
//! `stellar-cli.exe` (a console application: the prompt waits for it).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        // Windowed executable: redirect to stellar-cli.
        std::process::exit(stellar_app::console::redirect());
    }
    stellar_app::run_gui()
}
