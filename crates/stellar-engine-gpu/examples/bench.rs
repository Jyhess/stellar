//! Banc d'essai CPU / GPU sur un nuage sphérique au repos :
//! `cargo run --release -p stellar-engine-gpu --example bench -- 1000000 10`
//! (nombre de corps, nombre de pas ; ajouter `gpu` ou `cpu` pour n'en tester qu'un).
use std::time::Instant;

use stellar_application::generator::generate_state;
use stellar_domain::{InitialConditions, PhysicsEngine, PhysicsParams};
use stellar_engine_cpu::CpuEngine;
use stellar_engine_gpu::GpuEngine;

fn run(label: &str, mut engine: Box<dyn PhysicsEngine>, steps: usize, created: f64) {
    println!("{label} : création (premier calcul des forces) {created:.2} s");
    let t = Instant::now();
    let mut merges = 0;
    for _ in 0..steps {
        merges += engine.step().merges.len();
    }
    let per = t.elapsed().as_secs_f64() / steps as f64;
    println!(
        "{label} : {:.1} ms par pas ({:.1} pas/s) · {} fusions · {} corps restants",
        per * 1e3,
        1.0 / per,
        merges,
        engine.body_count()
    );
    let t = Instant::now();
    let s = engine.state();
    println!(
        "{label} : relecture complète de l'état : {:.1} ms ({} corps)",
        t.elapsed().as_secs_f64() * 1e3,
        s.bodies.len()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: usize = args
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let steps: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let only = args.get(2).map(String::as_str);
    let ic = InitialConditions {
        count: n,
        ..Default::default()
    };
    let params = PhysicsParams::suggested(&ic, 1.0);
    println!("{n} corps, {steps} pas");
    if only != Some("cpu") {
        let t = Instant::now();
        match GpuEngine::new(generate_state(&ic, 1.0), params) {
            Ok(e) => {
                println!("GPU : {}", e.adapter_name());
                let created = t.elapsed().as_secs_f64();
                run("GPU", Box::new(e), steps, created);
            }
            Err(e) => println!("GPU indisponible : {e}"),
        }
    }
    if only != Some("gpu") {
        let t = Instant::now();
        let e = CpuEngine::new(generate_state(&ic, 1.0), params);
        let created = t.elapsed().as_secs_f64();
        run("CPU", Box::new(e), steps, created);
    }
}
