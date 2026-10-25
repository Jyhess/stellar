//! Mesure rapide du moteur :
//! `cargo run --release -p stellar-engine-cpu --example timing -- <particules> <pas>`
use std::time::Instant;

use stellar_application::generator::generate_state;
use stellar_domain::{InitialConditions, PhysicsEngine, PhysicsParams};
use stellar_engine_cpu::CpuEngine;

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let steps: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);
    let ic = InitialConditions { count: n, ..Default::default() };
    let t = Instant::now();
    let s = generate_state(&ic, 1.0);
    println!("génération : {:?}", t.elapsed());
    let t = Instant::now();
    let mut e = CpuEngine::new(s, PhysicsParams::suggested(&ic, 1.0));
    println!("création (1 calcul de forces) : {:?}", t.elapsed());
    let e0 = e.diagnostics().total_energy();
    let (mut merges, mut removed) = (0usize, 0usize);
    let every = (steps / 10).max(1);
    let t_all = Instant::now();
    for k in 1..=steps {
        let t = Instant::now();
        let r = e.step();
        merges += r.merges.len();
        removed += r.removed.len();
        if k % every == 0 || k == steps {
            println!(
                "pas {k:>6} : {:>8.1} ms · corps {:>8} · fusions {merges:>8} · éliminés {removed:>6} · ΔE/E₀ {:+.3e}",
                t.elapsed().as_secs_f64() * 1e3,
                e.body_count(),
                (e.diagnostics().total_energy() - e0) / e0.abs()
            );
        }
    }
    println!("total : {:?}", t_all.elapsed());
}
