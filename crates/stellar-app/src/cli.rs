//! Stellar's command line: simulation without a window, resumption of a
//! save or a recording (`stellar-cli --help`).

fn main() {
    stellar_app::i18n::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(stellar_app::console::run(&args));
}
