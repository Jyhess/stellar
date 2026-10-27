//! Tests du moteur GPU. Ignorés (avec un message) si aucune carte graphique
//! compatible n'est disponible.
//!
//! Sans carte graphique (intégration continue), on peut utiliser le rendu
//! logiciel : `WGPU_BACKEND=gl xvfb-run cargo test -p stellar-engine-gpu`.

use glam::DVec3;
use stellar_application::generator::generate_state;
use stellar_domain::{
    BodySet, Distribution, InitialConditions, PhysicsEngine, PhysicsParams, SimulationState,
    VelocityMode,
};
use stellar_engine_cpu::CpuEngine;
use stellar_engine_gpu::GpuEngine;

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
    SimulationState {
        bodies,
        ..Default::default()
    }
}

/// Construit le moteur, ou `None` (test ignoré) sans carte graphique.
fn engine(s: SimulationState, p: PhysicsParams) -> Option<GpuEngine> {
    match GpuEngine::new(s, p) {
        Ok(e) => Some(e),
        Err(msg) if msg.contains("carte graphique") => {
            eprintln!("test GPU ignoré : {msg}");
            None
        }
        Err(msg) => panic!("{msg}"),
    }
}

fn sphere(count: usize, seed: u64) -> SimulationState {
    let ic = InitialConditions {
        count,
        radius: 10.0,
        seed,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Rest,
    };
    generate_state(&ic, 1.0)
}

#[test]
fn barnes_hut_matches_direct_sum() {
    let s = sphere(3_000, 1);
    let bodies = s.bodies.clone();
    let mut p = params();
    p.softening = 0.05;
    let Some(e) = engine(s, p) else { return };
    let eps2 = p.softening * p.softening;
    let mut errors: Vec<f64> = e
        .read_bodies()
        .iter()
        .map(|g| {
            let i = g.id as usize;
            let mut exact = DVec3::ZERO;
            for j in 0..bodies.len() {
                if j != i {
                    let d = bodies.positions[j] - bodies.positions[i];
                    let r2 = d.length_squared() + eps2;
                    exact += d * (bodies.masses[j] / (r2 * r2.sqrt()));
                }
            }
            let a = DVec3::new(g.acc[0] as f64, g.acc[1] as f64, g.acc[2] as f64);
            (a - exact).length() / exact.length()
        })
        .collect();
    assert_eq!(errors.len(), 3_000);
    errors.sort_by(f64::total_cmp);
    let median = errors[errors.len() / 2];
    let p99 = errors[errors.len() * 99 / 100];
    eprintln!("Barnes-Hut GPU : erreur médiane {median:.2e}, 99e centile {p99:.2e}");
    assert!(median < 1e-2, "erreur médiane {median}");
    assert!(p99 < 5e-2, "erreur 99e centile {p99}");
}

#[test]
fn agrees_with_cpu_engine_over_steps() {
    let ic = InitialConditions {
        count: 2_000,
        radius: 10.0,
        seed: 3,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Random { min: 0.0, max: 2.0 },
    };
    let s = generate_state(&ic, 1.0);
    let mut p = PhysicsParams::suggested(&ic, 1.0);
    p.base_radius = 0.0;
    p.cleanup_period = 0;
    let Some(mut gpu) = engine(s.clone(), p) else {
        return;
    };
    let mut cpu = CpuEngine::new(s, p);
    for _ in 0..50 {
        gpu.step();
        cpu.step();
    }
    let (sg, sc) = (gpu.state(), cpu.state());
    let pos_c: std::collections::HashMap<u64, DVec3> = sc
        .bodies
        .ids
        .iter()
        .copied()
        .zip(sc.bodies.positions.iter().copied())
        .collect();
    let mut worst = 0.0f64;
    for (id, p) in sg.bodies.ids.iter().zip(&sg.bodies.positions) {
        worst = worst.max((*p - pos_c[id]).length());
    }
    // Écart dû à la simple précision et aux arbres différents : bien
    // inférieur à la distance moyenne entre particules (~1,3).
    assert!(worst < 0.05, "écart maximal CPU/GPU : {worst}");
    let (dg, dc) = (gpu.diagnostics(), cpu.diagnostics());
    let rel = (dg.total_energy() - dc.total_energy()).abs() / dc.total_energy().abs();
    eprintln!(
        "CPU/GPU après 50 pas : écart de position max {worst:.2e}, écart d'énergie {rel:.2e}"
    );
    assert!(
        rel < 1e-3,
        "énergies : GPU {} / CPU {}",
        dg.total_energy(),
        dc.total_energy()
    );
}

#[test]
fn two_body_orbit_conserves_energy() {
    let mut b = BodySet::default();
    b.push(0, DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, 0.5, 0.0), 1.0);
    b.push(
        1,
        DVec3::new(-1.0, 0.0, 0.0),
        DVec3::new(0.0, -0.5, 0.0),
        1.0,
    );
    let mut p = params();
    p.dt = 1e-2;
    let Some(mut e) = engine(state(b), p) else {
        return;
    };
    let e0 = e.diagnostics().total_energy();
    assert!((e0 + 0.25).abs() < 1e-5, "{e0}");
    for _ in 0..2_000 {
        e.step();
    }
    let e1 = e.diagnostics().total_energy();
    eprintln!(
        "orbite à deux corps : dérive d'énergie {:.2e}",
        (e1 - e0) / e0
    );
    assert!(((e1 - e0) / e0).abs() < 1e-3, "dérive {}", (e1 - e0) / e0);
}

#[test]
fn head_on_collision_merges_and_conserves_momentum() {
    let mut b = BodySet::default();
    b.push(
        10,
        DVec3::new(-1.0, 0.0, 0.0),
        DVec3::new(2.0, 0.0, 0.0),
        1.0,
    );
    b.push(
        20,
        DVec3::new(1.0, 0.0, 0.0),
        DVec3::new(-1.0, 0.3, 0.0),
        2.0,
    );
    let p_before = b.momentum();
    let mut p = params();
    p.g = 0.0;
    p.dt = 0.1;
    p.base_radius = 0.1;
    let Some(mut e) = engine(state(b), p) else {
        return;
    };
    let mut merges = Vec::new();
    for _ in 0..20 {
        merges.extend(e.step().merges);
    }
    assert_eq!(merges, vec![(10, 20)]);
    let s = e.state();
    assert_eq!(s.bodies.len(), 1);
    assert_eq!(s.bodies.ids[0], 20);
    assert_eq!(s.bodies.masses[0], 3.0);
    assert!((s.bodies.momentum() - p_before).length() < 1e-5);
}

#[test]
fn fast_bodies_crossing_within_one_step_merge() {
    let mut b = BodySet::default();
    b.push(
        1,
        DVec3::new(-5.0, 0.0, 0.0),
        DVec3::new(100.0, 0.0, 0.0),
        1.0,
    );
    b.push(
        2,
        DVec3::new(5.0, 0.05, 0.0),
        DVec3::new(-100.0, 0.0, 0.0),
        1.0,
    );
    let mut p = params();
    p.g = 0.0;
    p.dt = 0.1;
    p.base_radius = 0.05;
    let Some(mut e) = engine(state(b), p) else {
        return;
    };
    assert_eq!(e.step().merges.len(), 1);
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
    let Some(mut e) = engine(state(b), p) else {
        return;
    };
    assert_eq!(e.step().merges.len(), 4);
    let s = e.state();
    assert_eq!(s.bodies.len(), 1);
    assert_eq!(s.bodies.masses[0], 5.0);
    assert!((s.bodies.positions[0].x - 0.3).abs() < 1e-5);
}

#[test]
fn cleanup_removes_only_unbound_distant_bodies() {
    let mut b = BodySet::default();
    for i in 0..50 {
        let a = i as f64 * 0.7;
        b.push(
            i,
            DVec3::new(a.cos(), a.sin(), (a * 0.3).sin()) * 0.5,
            DVec3::ZERO,
            1.0,
        );
    }
    b.push(100, DVec3::new(30.0, 0.0, 0.0), DVec3::ZERO, 1.0);
    b.push(
        200,
        DVec3::new(0.0, 30.0, 0.0),
        DVec3::new(0.0, 50.0, 0.0),
        1.0,
    );
    let mut p = params();
    p.softening = 0.01;
    p.cleanup_period = 1;
    let Some(mut e) = engine(state(b), p) else {
        return;
    };
    let r = e.step();
    assert_eq!(r.removed, vec![200]);
    assert_eq!(e.body_count(), 51);
    let ids = e.state().bodies.ids;
    assert!(ids.contains(&100) && !ids.contains(&200));
}

#[test]
fn collapse_conserves_mass_and_ids() {
    let ic = InitialConditions {
        count: 3_000,
        radius: 10.0,
        seed: 5,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Rest,
    };
    let mut p = PhysicsParams::suggested(&ic, 1.0);
    p.base_radius *= 20.0;
    p.cleanup_period = 0;
    let Some(mut e) = engine(generate_state(&ic, 1.0), p) else {
        return;
    };
    let mut merged = 0;
    for _ in 0..600 {
        merged += e.step().merges.len();
    }
    let s = e.state();
    assert!(merged > 0);
    assert_eq!(s.bodies.len() + merged, 3_000);
    assert!((s.bodies.total_mass() - 3_000.0).abs() < 1e-3);
    let mut ids = s.bodies.ids.clone();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), s.bodies.len());
}

#[test]
fn queries_positions_and_barycenter() {
    let mut b = BodySet::default();
    b.push(7, DVec3::new(1.0, 2.0, 3.0), DVec3::ZERO, 1.0);
    b.push(9, DVec3::new(-1.0, 0.0, 1.0), DVec3::ZERO, 3.0);
    let mut p = params();
    p.g = 0.0;
    let Some(e) = engine(state(b), p) else { return };
    let pos = e.positions_of(&[9, 42, 7]);
    assert_eq!(pos[0], Some(DVec3::new(-1.0, 0.0, 1.0)));
    assert_eq!(pos[1], None);
    assert_eq!(pos[2], Some(DVec3::new(1.0, 2.0, 3.0)));
    let c = e.center_of_mass().unwrap();
    assert!((c - DVec3::new(-0.5, 0.5, 1.5)).length() < 1e-6);
}

/// Carte partagée : les tampons exposés à l'affichage contiennent, aux
/// décalages annoncés, exactement les positions et masses de l'état.
#[test]
fn shared_device_exposes_current_bodies() {
    use stellar_engine_gpu::{BodiesView, SharedDevice};

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::from_env()
            .unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL),
        ..Default::default()
    });
    let Some(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("test GPU ignoré : aucune carte graphique");
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("ouverture de la carte graphique");
    let shared = SharedDevice {
        adapter,
        device: device.clone(),
        queue: queue.clone(),
    };

    // Nuage qui s'effondre, gros rayons : fusions, donc compactions.
    let ic = InitialConditions {
        count: 2_000,
        radius: 10.0,
        seed: 11,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Rest,
    };
    let mut p = PhysicsParams::suggested(&ic, 1.0);
    p.base_radius *= 20.0;
    let mut e = match GpuEngine::with_device(generate_state(&ic, 1.0), p, &shared) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("test GPU ignoré : {msg}");
            return;
        }
    };
    let view = e
        .live_view()
        .expect("vue exposée sur carte partagée")
        .downcast::<BodiesView>()
        .expect("vue du moteur GPU");

    let mut merged = 0;
    for _ in 0..300 {
        merged += e.step().merges.len();
    }
    assert!(merged > 0, "le test doit provoquer des fusions");

    let (buffer, count) = view.current();
    assert_eq!(count as usize, e.body_count());
    let size = count as u64 * BodiesView::STRIDE;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    queue.submit(Some(enc.finish()));
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    let _ = device.poll(wgpu::Maintain::Wait);
    let raw = staging.slice(..).get_mapped_range().to_vec();

    let s = e.state();
    let stride = BodiesView::STRIDE as usize;
    for i in 0..count as usize {
        let f = |off: usize| f32::from_le_bytes(raw[off..off + 4].try_into().unwrap()) as f64;
        let o = i * stride + BodiesView::POSITION_OFFSET as usize;
        let pos = DVec3::new(f(o), f(o + 4), f(o + 8));
        let mass = f(i * stride + BodiesView::MASS_OFFSET as usize);
        assert_eq!(pos, s.bodies.positions[i]);
        assert_eq!(mass, s.bodies.masses[i]);
        assert!(mass > 0.0, "corps mort dans l'état publié");
    }

    // Sans carte partagée : pas de vue.
    let own = GpuEngine::new(sphere(100, 3), params()).expect("carte graphique");
    assert!(own.live_view().is_none());
}
