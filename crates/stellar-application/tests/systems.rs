//! Moteur composite : sous-systèmes couplés par leurs champs de marée.

use std::sync::Arc;

use stellar_application::MultiSystem;
use stellar_domain::{
    BodySet, PhysicsEngine, PhysicsParams, SimulationState, SystemSpan, Vec3,
};
use stellar_engine_cpu::{CpuEngine, CpuEngineFactory};

fn params() -> PhysicsParams {
    PhysicsParams {
        g: 1.0,
        dt: 1e-3,
        softening: 0.2,
        // Forces quasi exactes : les écarts mesurés sont ceux du découpage.
        theta: 0.05,
        base_radius: 0.0,
        cleanup_period: 0,
        cleanup_distance_factor: 10.0,
        reference_radius: 1.0,
        cluster_link_length: 1.0,
        cluster_min_bodies: 10,
        cluster_min_mass: 0.0,
    }
}

/// Amas de `n` corps de masse 1 dans une boule de rayon `radius`.
fn cluster(b: &mut BodySet, first: u64, n: u64, center: Vec3, radius: f64, velocity: Vec3) {
    for k in 0..n {
        let a = k as f64 * 2.399_963;
        let z = 1.0 - 2.0 * (k as f64 + 0.5) / n as f64;
        let s = (1.0 - z * z).sqrt();
        let r = radius * ((k as f64 + 0.5) / n as f64).cbrt();
        b.push(
            first + k,
            center + Vec3::new(a.cos() * s, a.sin() * s, z) * r,
            velocity,
            1.0,
        );
    }
}

fn composite(bodies: BodySet, systems: Vec<SystemSpan>, p: PhysicsParams) -> MultiSystem {
    let state = SimulationState {
        initial_mass: Some(bodies.total_mass()),
        bodies,
        systems,
        ..Default::default()
    };
    MultiSystem::new(Arc::new(CpuEngineFactory), state, p).expect("moteur composite")
}

fn center_of(e: &dyn PhysicsEngine, first: u64, n: u64) -> Vec3 {
    let ids: Vec<u64> = (first..first + n).collect();
    let p = e.positions_of(&ids);
    p.iter().map(|p| p.expect("corps présent")).sum::<Vec3>() / n as f64
}

/// Deux amas compacts en orbite : le calcul en deux sous-systèmes suit le
/// calcul d'un seul tenant.
#[test]
fn split_orbit_follows_the_full_computation() {
    let d: f64 = 20.0;
    let (ma, mb) = (50.0, 20.0);
    let v = ((ma + mb) / d).sqrt();
    let mut b = BodySet::default();
    cluster(&mut b, 0, 50, Vec3::ZERO, 0.3, Vec3::new(0.0, -v * mb / (ma + mb), 0.0));
    cluster(&mut b, 100, 20, Vec3::new(d, 0.0, 0.0), 0.2, Vec3::new(0.0, v * ma / (ma + mb), 0.0));
    let spans = vec![
        SystemSpan { start: 0, len: 50, size: 1.0 },
        SystemSpan { start: 50, len: 20, size: 0.5 },
    ];
    let mut split = composite(b.clone(), spans, params());
    let mut whole = CpuEngine::new(
        SimulationState {
            bodies: b,
            ..Default::default()
        },
        params(),
    );
    assert_eq!(split.systems().len(), 2);
    for _ in 0..3_000 {
        split.step();
        whole.step();
    }
    let rel_split = center_of(&split, 100, 20) - center_of(&split, 0, 50);
    let rel_whole = center_of(&whole, 100, 20) - center_of(&whole, 0, 50);
    // Trois unités de temps : l'amas a parcouru plus de 5 unités sur son orbite.
    assert!((rel_whole - Vec3::new(d, 0.0, 0.0)).length() > 5.0);
    assert!(
        (rel_split - rel_whole).length() < 1e-2,
        "{rel_split} ≠ {rel_whole}"
    );
    // Énergie totale : propres + interaction.
    let (es, ew) = (split.diagnostics().total_energy(), whole.diagnostics().total_energy());
    assert!(((es - ew) / ew).abs() < 1e-2, "{es} ≠ {ew}");
}

/// Un amas éjecté lié devient un sous-système au nettoyage.
#[test]
fn ejected_cluster_becomes_a_subsystem() {
    let mut b = BodySet::default();
    cluster(&mut b, 0, 50, Vec3::ZERO, 0.5, Vec3::ZERO);
    cluster(&mut b, 1_000, 20, Vec3::new(0.0, 30.0, 0.0), 0.2, Vec3::new(0.0, 50.0, 0.0));
    b.push(200, Vec3::new(-30.0, 0.0, 0.0), Vec3::new(-50.0, 0.0, 0.0), 1.0);
    let mut p = params();
    p.softening = 0.05;
    p.cleanup_period = 1;
    let mut e = composite(b, Vec::new(), p);
    assert_eq!(e.systems().len(), 1);
    let r = e.step();
    assert_eq!(r.removed, vec![200]);
    let systems = e.systems();
    assert_eq!(systems.len(), 2);
    assert_eq!(systems[1].bodies, 20);
    assert!((systems[1].center.y - 30.05).abs() < 0.1);
    let s = e.state();
    assert_eq!(s.bodies.len(), 70);
    assert_eq!(s.systems.len(), 2);
    assert_eq!(s.systems[1].len, 20);
    // L'état composite reconstruit les mêmes sous-systèmes.
    let again = composite(s.bodies.clone(), s.systems.clone(), p);
    assert_eq!(again.systems().len(), 2);
    assert!((center_of(&again, 1_000, 20) - center_of(&e, 1_000, 20)).length() < 1e-9);
    // Rien n'est perdu ensuite, hors éliminations signalées.
    let mut removed = 0;
    for _ in 0..50 {
        removed += e.step().removed.len();
    }
    assert_eq!(e.body_count() + removed, 70);
}

/// Deux sous-systèmes qui se rapprochent sont réunis.
#[test]
fn approaching_subsystems_merge() {
    let mut b = BodySet::default();
    cluster(&mut b, 0, 50, Vec3::ZERO, 0.3, Vec3::ZERO);
    cluster(&mut b, 100, 20, Vec3::new(6.0, 0.0, 0.0), 0.2, Vec3::new(-5.0, 0.0, 0.0));
    let spans = vec![
        SystemSpan { start: 0, len: 50, size: 1.0 },
        SystemSpan { start: 50, len: 20, size: 0.5 },
    ];
    let mut p = params();
    p.cleanup_period = 10;
    let mut e = composite(b, spans, p);
    let mut steps = 0;
    while e.systems().len() > 1 && steps < 2_000 {
        e.step();
        steps += 1;
    }
    assert_eq!(e.systems().len(), 1, "pas de réunion");
    // Réunion quand 1,5 > 0,6 × distance, soit à moins de 2,5 : après
    // environ 0,6 unité de temps (le premier nettoyage suivant).
    assert!((500..900).contains(&steps), "{steps} pas");
    assert_eq!(e.body_count(), 70);
    assert!(e.state().systems.is_empty());
}

/// Un corps qui s'évade d'un sous-système secondaire rejoint le principal.
#[test]
fn escapee_joins_the_main_system() {
    let mut b = BodySet::default();
    cluster(&mut b, 0, 50, Vec3::ZERO, 0.5, Vec3::ZERO);
    cluster(&mut b, 100, 20, Vec3::new(40.0, 0.0, 0.0), 0.2, Vec3::ZERO);
    // Loin de son amas (au-delà de 10 × 0,5) et bien trop rapide pour lui.
    b.push(500, Vec3::new(40.0, 8.0, 0.0), Vec3::new(0.0, 20.0, 0.0), 1.0);
    let spans = vec![
        SystemSpan { start: 0, len: 50, size: 1.0 },
        SystemSpan { start: 50, len: 21, size: 0.5 },
    ];
    let mut p = params();
    p.cleanup_period = 1;
    let mut e = composite(b, spans, p);
    let r = e.step();
    assert!(r.removed.is_empty(), "{:?}", r.removed);
    assert_eq!(e.body_count(), 71);
    let systems = e.systems();
    assert_eq!(systems.len(), 2);
    assert_eq!(systems[0].bodies, 51);
    assert_eq!(systems[1].bodies, 20);
    let p = e.positions_of(&[500])[0].expect("évadé présent");
    assert!((p - Vec3::new(40.0, 8.02, 0.0)).length() < 0.05, "{p}");
}
