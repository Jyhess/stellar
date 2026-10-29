//! Ligne de commande de Stellar : simulation sans fenêtre, reprise d'une
//! sauvegarde ou d'un enregistrement (`stellar-cli --help`).

fn main() {
    stellar_app::i18n::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(stellar_app::console::run(&args));
}
