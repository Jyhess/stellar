//! Interface graphique de Stellar. La ligne de commande est servie par
//! `stellar-cli.exe` (application console : l'invite attend sa fin).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        // Exécutable fenêtré : renvoi vers stellar-cli.
        std::process::exit(stellar_app::console::redirect());
    }
    stellar_app::run_gui()
}
