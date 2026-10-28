//! Coût de l'interpolation d'une image de relecture :
//! `cargo run --release -p stellar-application --example interp_timing -- 1000000`
use std::time::Instant;

use stellar_application::recording::Keyframe;
use stellar_application::replay::{build_targets, interpolate};
use stellar_domain::{BodySet, Vec3};

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let mk = |k: f64| {
        let mut b = BodySet::with_capacity(n);
        for i in 0..n {
            let f = i as f64 * 1e-3 + k;
            b.push(
                i as u64,
                Vec3::new(f.sin(), f.cos(), f),
                Vec3::new(f.cos(), -f.sin(), 1.0),
                1.0,
            );
        }
        Keyframe {
            step: k as u64,
            time: k,
            bodies: b,
            fates: vec![],
            dust_weight: 1.0,
            groups: Vec::new(),
        }
    };
    let (a, b) = (mk(0.0), mk(1.0));
    let t = Instant::now();
    let targets = build_targets(&a, &b);
    println!("appariement (une fois par paire) : {:?}", t.elapsed());
    let mut out = BodySet::default();
    interpolate(&a, &b, &targets, 0.5, &mut out);
    let t = Instant::now();
    for k in 0..20 {
        interpolate(&a, &b, &targets, k as f64 / 20.0, &mut out);
    }
    println!("interpolation : {:?} par image", t.elapsed() / 20);
}
