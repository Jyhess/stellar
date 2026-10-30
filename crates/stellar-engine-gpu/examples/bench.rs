//! CPU / GPU benchmark on a spherical cloud at rest:
//! `cargo run --release -p stellar-engine-gpu --example bench -- 1000000 10`
//! (body count, step count; add `gpu` or `cpu` to test only one engine, and
//! `disc` for a rotating disc, like `STELLAR_AUTODISK`).
//!
//! With `STELLAR_GPU_PROFILE=1`, the time of each GPU kernel is printed.
use std::time::Instant;

use stellar_application::generator::generate_state;
use stellar_domain::{
    DensityProfile, Distribution, InitialConditions, PhysicsEngine, PhysicsParams, VelocityMode,
};
use stellar_engine_cpu::CpuEngine;
use stellar_engine_gpu::GpuEngine;

fn run(label: &str, engine: &mut dyn PhysicsEngine, steps: usize, created: f64) {
    println!("{label}: creation (first force computation) {created:.2} s");
    let t = Instant::now();
    let mut merges = 0;
    for _ in 0..steps {
        merges += engine.step().merges.len();
    }
    let per = t.elapsed().as_secs_f64() / steps as f64;
    println!(
        "{label}: {:.1} ms per step ({:.1} steps/s) · {} mergers · {} bodies left",
        per * 1e3,
        1.0 / per,
        merges,
        engine.body_count()
    );
    let t = Instant::now();
    let s = engine.state();
    println!(
        "{label}: full state readback: {:.1} ms ({} bodies)",
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
    let mut ic = InitialConditions {
        count: n,
        ..Default::default()
    };
    if args.iter().any(|a| a == "disc") {
        ic.distribution = Distribution::Disk {
            thickness: 0.05,
            profile: DensityProfile::Exponential { scale: 0.3 },
        };
        ic.velocity = VelocityMode::CircularOrbital { spread: 0.05, factor: 1.0 };
    }
    let params = PhysicsParams::suggested(&ic, 1.0);
    println!("{n} bodies, {steps} steps");
    if only != Some("cpu") {
        let t = Instant::now();
        match GpuEngine::new(generate_state(&ic, 1.0), params) {
            Ok(mut e) => {
                println!("GPU : {}", e.adapter_name());
                let created = t.elapsed().as_secs_f64();
                run("GPU", &mut e, steps, created);
                let profile = e.profile();
                if !profile.is_empty() {
                    println!("GPU profile (time per step, STELLAR_GPU_PROFILE profiling):");
                    for (name, ms, count) in profile {
                        println!(
                            "  {name:<16} {:>8.3} ms  ({} dispatches per step)",
                            ms / (steps + 1) as f64,
                            count as f64 / (steps + 1) as f64
                        );
                    }
                }
            }
            Err(e) => println!("GPU unavailable: {e}"),
        }
    }
    if only != Some("gpu") {
        let t = Instant::now();
        let e = CpuEngine::new(generate_state(&ic, 1.0), params);
        let created = t.elapsed().as_secs_f64();
        run("CPU", &mut { e }, steps, created);
    }
}
