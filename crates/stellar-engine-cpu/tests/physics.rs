//! Tests physiques du moteur CPU.

use glam::DVec3;
use stellar_application::generator::generate_state;
use stellar_domain::{
    BodySet, Distribution, InitialConditions, PhysicsEngine, PhysicsParams, SimulationState,
    VelocityMode,
};
use stellar_engine_cpu::CpuEngine;

fn params() -> PhysicsParams {
    PhysicsParams {
        g: 1.0,
        dt: 1e-3,
        softening: 0.0,
        theta: 0.5,
        base_radius: 0.0,
        cleanup_period: 0,
        cleanup_distance_factor: 10.0,
        reference_radius: 1.0,
    }
}

fn state(bodies: BodySet) -> SimulationState {
    SimulationState { bodies, ..Default::default() }
}

#[test]
fn barnes_hut_matches_direct_sum() {
    let ic = InitialConditions {
        count: 3_000,
        radius: 10.0,
        seed: 1,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Rest,
    };
    let s = generate_state(&ic, 1.0);
    let bodies = s.bodies.clone();
    let mut p = params();
    p.softening = 0.05;
    p.theta = 0.5;
    let engine = CpuEngine::new(s, p);
    let (ids, acc) = engine.accelerations();

    let eps2 = p.softening * p.softening;
    let mut errors: Vec<f64> = ids
        .iter()
        .zip(acc)
        .map(|(&id, a)| {
            let i = id as usize;
            let mut exact = DVec3::ZERO;
            for j in 0..bodies.len() {
                if j != i {
                    let d = bodies.positions[j] - bodies.positions[i];
                    let r2 = d.length_squared() + eps2;
                    exact += d * (bodies.masses[j] / (r2 * r2.sqrt()));
                }
            }
            (*a - exact).length() / exact.length()
        })
        .collect();
    errors.sort_by(f64::total_cmp);
    let median = errors[errors.len() / 2];
    let p99 = errors[errors.len() * 99 / 100];
    assert!(median < 5e-3, "erreur médiane {median}");
    assert!(p99 < 3e-2, "erreur 99e centile {p99}");
}

#[test]
fn two_body_orbit_conserves_energy() {
    // Orbite circulaire : masses 1, séparation 2, v = √(G·m_total/(4·r))... calcul direct :
    // chaque corps tourne à r = 1 autour du barycentre ; force G·1·1/4 = v²/1 ⇒ v = 0,5.
    let mut b = BodySet::default();
    b.push(0, DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, 0.5, 0.0), 1.0);
    b.push(1, DVec3::new(-1.0, 0.0, 0.0), DVec3::new(0.0, -0.5, 0.0), 1.0);
    let mut p = params();
    p.dt = 1e-2;
    let mut e = CpuEngine::new(state(b), p);
    let e0 = e.diagnostics().total_energy();
    assert!((e0 - (0.25 - 0.5)).abs() < 1e-12);
    for _ in 0..5_000 {
        e.step();
    }
    let e1 = e.diagnostics().total_energy();
    assert!(((e1 - e0) / e0).abs() < 1e-4, "dérive {}", (e1 - e0) / e0);
    let s = e.state();
    assert!(s.bodies.momentum().length() < 1e-12);
}

#[test]
fn head_on_collision_merges_and_conserves_momentum() {
    let mut b = BodySet::default();
    b.push(10, DVec3::new(-1.0, 0.0, 0.0), DVec3::new(2.0, 0.0, 0.0), 1.0);
    b.push(20, DVec3::new(1.0, 0.0, 0.0), DVec3::new(-1.0, 0.3, 0.0), 2.0);
    let p_before = b.momentum();
    let mut p = params();
    p.g = 0.0;
    p.dt = 0.1;
    p.base_radius = 0.1;
    let mut e = CpuEngine::new(state(b), p);
    let mut merges = Vec::new();
    for _ in 0..20 {
        merges.extend(e.step().merges);
    }
    assert_eq!(merges, vec![(10, 20)]);
    let s = e.state();
    assert_eq!(s.bodies.len(), 1);
    assert_eq!(s.bodies.ids[0], 20);
    assert_eq!(s.bodies.masses[0], 3.0);
    assert!((s.bodies.momentum() - p_before).length() < 1e-12);
}

#[test]
fn fast_bodies_crossing_within_one_step_merge() {
    // Sans balayage, ces corps se « traverseraient » sans jamais se recouvrir.
    let mut b = BodySet::default();
    b.push(1, DVec3::new(-5.0, 0.0, 0.0), DVec3::new(100.0, 0.0, 0.0), 1.0);
    b.push(2, DVec3::new(5.0, 0.05, 0.0), DVec3::new(-100.0, 0.0, 0.0), 1.0);
    let mut p = params();
    p.g = 0.0;
    p.dt = 0.1;
    p.base_radius = 0.05;
    let mut e = CpuEngine::new(state(b), p);
    let report = e.step();
    assert_eq!(report.merges.len(), 1);
    assert_eq!(e.body_count(), 1);
}

#[test]
fn chain_collision_merges_into_single_body() {
    let mut b = BodySet::default();
    for i in 0..5 {
        b.push(i, DVec3::new(i as f64 * 0.15, 0.0, 0.0), DVec3::ZERO, 1.0);
    }
    let mut p = params();
    p.g = 0.0;
    p.base_radius = 0.1;
    let mut e = CpuEngine::new(state(b), p);
    let r = e.step();
    assert_eq!(r.merges.len(), 4);
    let s = e.state();
    assert_eq!(s.bodies.len(), 1);
    assert_eq!(s.bodies.masses[0], 5.0);
    assert!((s.bodies.positions[0].x - 0.3).abs() < 1e-9);
}

#[test]
fn cleanup_removes_only_unbound_distant_bodies() {
    let mut b = BodySet::default();
    // Un amas central lié…
    for i in 0..50 {
        let a = i as f64 * 0.7;
        b.push(i, DVec3::new(a.cos(), a.sin(), (a * 0.3).sin()) * 0.5, DVec3::ZERO, 1.0);
    }
    // … un corps lointain mais lié (au repos) …
    b.push(100, DVec3::new(30.0, 0.0, 0.0), DVec3::ZERO, 1.0);
    // … et un corps lointain non lié (très rapide).
    b.push(200, DVec3::new(0.0, 30.0, 0.0), DVec3::new(0.0, 50.0, 0.0), 1.0);
    let mut p = params();
    p.softening = 0.01;
    p.cleanup_period = 1;
    p.cleanup_distance_factor = 10.0;
    p.reference_radius = 1.0;
    let mut e = CpuEngine::new(state(b), p);
    let r = e.step();
    assert_eq!(r.removed, vec![200]);
    assert_eq!(e.body_count(), 51);
}

#[test]
fn collapse_conserves_mass_through_merges() {
    let ic = InitialConditions {
        count: 2_000,
        radius: 10.0,
        seed: 5,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Rest,
    };
    let initial = generate_state(&ic, 1.0);
    let mut p = PhysicsParams::suggested(&ic, 1.0);
    p.base_radius *= 20.0; // fusions fréquentes
    p.cleanup_period = 0;
    let mut e = CpuEngine::new(initial, p);
    let mut merged = 0;
    for _ in 0..600 {
        merged += e.step().merges.len();
    }
    let s = e.state();
    assert!(merged > 0, "aucune fusion observée");
    assert_eq!(s.bodies.len() + merged, 2_000);
    assert!((s.bodies.total_mass() - 2_000.0).abs() < 1e-9);
    // Barnes-Hut n'est pas exactement antisymétrique : tolérance relative à
    // l'échelle M·v_caractéristique.
    let scale = 2_000.0 * ic.characteristic_speed(1.0);
    assert!(s.bodies.momentum().length() < 1e-3 * scale, "P = {}", s.bodies.momentum().length());
    // Identifiants uniques.
    let mut ids = s.bodies.ids.clone();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), s.bodies.len());
}
