//! Taille et coût d'un instantané d'enregistrement :
//! `cargo run --release -p stellar-persistence --example frame_size -- 1000000`
use std::time::Instant;

use stellar_application::generator::generate_state;
use stellar_application::recording::Keyframe;
use stellar_domain::{InitialConditions, VelocityMode};
use stellar_persistence::recording::{decode_frame, encode_frame, VERSION};

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let ic = InitialConditions {
        count: n,
        velocity: VelocityMode::CircularOrbital { spread: 0.1 },
        ..Default::default()
    };
    let s = generate_state(&ic, 1.0);
    let frame = Keyframe {
        step: 0,
        time: 0.0,
        bodies: s.bodies,
        fates: vec![],
        dust_weight: 1.0,
        groups: Vec::new(),
    };
    let t = Instant::now();
    let raw = encode_frame(&frame, VERSION);
    let packed = zstd::bulk::compress(&raw, 3).unwrap();
    println!("codage + compression : {:?}", t.elapsed());
    println!(
        "taille : {:.1} Mo brut, {:.1} Mo compressé ({:.1} octets/corps)",
        raw.len() as f64 / 1e6,
        packed.len() as f64 / 1e6,
        packed.len() as f64 / n as f64
    );
    let t = Instant::now();
    let unpacked = zstd::bulk::decompress(&packed, raw.len()).unwrap();
    let meta = stellar_application::FrameMeta {
        step: 0,
        time: 0.0,
        count: n as u32,
    };
    let d = decode_frame(meta, &unpacked, VERSION).unwrap();
    println!(
        "décompression + décodage : {:?} ({} corps)",
        t.elapsed(),
        d.bodies.len()
    );
}
