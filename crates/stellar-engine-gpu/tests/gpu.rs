//! Tests of the GPU engine. Skipped (with a message) if no compatible
//! graphics card is available.
//!
//! Without a graphics card (continuous integration), software rendering can
//! be used: `WGPU_BACKEND=gl xvfb-run cargo test -p stellar-engine-gpu`.

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
        cluster_link_length: 1.0,
        cluster_min_bodies: 10,
        cluster_min_mass: 0.0,
    }
}

fn state(bodies: BodySet) -> SimulationState {
    SimulationState {
        bodies,
        ..Default::default()
    }
}

/// Builds the engine, or `None` (test skipped) without a graphics card.
fn engine(s: SimulationState, p: PhysicsParams) -> Option<GpuEngine> {
    match GpuEngine::new(s, p) {
        Ok(e) => Some(e),
        Err(msg) if msg.contains("carte graphique") => {
            eprintln!("GPU test skipped: {msg}");
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
        ..Default::default()
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
    let mut worst_phi = 0.0f64;
    let mut errors: Vec<f64> = e
        .read_bodies()
        .iter()
        .map(|g| {
            let i = g.id as usize;
            let mut exact = DVec3::ZERO;
            let mut phi = 0.0;
            for j in 0..bodies.len() {
                if j != i {
                    let d = bodies.positions[j] - bodies.positions[i];
                    let r2 = d.length_squared() + eps2;
                    exact += d * (bodies.masses[j] / (r2 * r2.sqrt()));
                    phi -= bodies.masses[j] / r2.sqrt();
                }
            }
            let a = DVec3::new(g.acc[0] as f64, g.acc[1] as f64, g.acc[2] as f64);
            worst_phi = worst_phi.max((g.pot as f64 - phi).abs() / phi.abs());
            (a - exact).length() / exact.length()
        })
        .collect();
    eprintln!("GPU Barnes-Hut: maximum potential error {worst_phi:.2e}");
    assert!(worst_phi < 1e-2, "potential error {worst_phi}");
    assert_eq!(errors.len(), 3_000);
    errors.sort_by(f64::total_cmp);
    let median = errors[errors.len() / 2];
    let p99 = errors[errors.len() * 99 / 100];
    eprintln!("GPU Barnes-Hut: median error {median:.2e}, 99th percentile {p99:.2e}");
    assert!(median < 1e-2, "median error {median}");
    assert!(p99 < 5e-2, "99th percentile error {p99}");
}

#[test]
fn agrees_with_cpu_engine_over_steps() {
    let ic = InitialConditions {
        count: 2_000,
        radius: 10.0,
        seed: 3,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Random { min: 0.0, max: 2.0 },
        ..Default::default()
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
    // Gap due to single precision and to the different trees: well below the
    // mean distance between particles (~1.3).
    assert!(worst < 0.05, "maximum CPU/GPU difference: {worst}");
    let (dg, dc) = (gpu.diagnostics(), cpu.diagnostics());
    let rel = (dg.total_energy() - dc.total_energy()).abs() / dc.total_energy().abs();
    eprintln!(
        "CPU/GPU after 50 steps: max position difference {worst:.2e}, energy difference {rel:.2e}"
    );
    assert!(
        rel < 1e-3,
        "energies: GPU {} / CPU {}",
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
        "two-body orbit: energy drift {:.2e}",
        (e1 - e0) / e0
    );
    assert!(((e1 - e0) / e0).abs() < 1e-3, "drift {}", (e1 - e0) / e0);
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
        ..Default::default()
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

/// Tracked positions: read back with every step (no separate readback),
/// consistent with the state, corrected for the step's mergers (absorbed
/// body absent, resulting body at the barycenter); the center of mass
/// follows too.
#[test]
fn tracked_positions_follow_steps_and_merges() {
    let mut b = BodySet::default();
    b.push(10, DVec3::new(-1.0, 0.0, 0.0), DVec3::new(2.0, 0.0, 0.0), 1.0);
    b.push(20, DVec3::new(1.0, 0.0, 0.0), DVec3::new(-1.0, 0.3, 0.0), 2.0);
    b.push(30, DVec3::new(0.0, 50.0, 0.0), DVec3::new(0.0, 0.0, 1.0), 1.0);
    let mut p = params();
    p.g = 0.0;
    p.dt = 0.1;
    p.base_radius = 0.1;
    let Some(mut e) = engine(state(b), p) else {
        return;
    };
    let ids = [10, 20, 30, 99];
    assert_eq!(e.positions_of(&ids)[3], None);
    let mut merged = false;
    for _ in 0..20 {
        let report = e.step();
        let pos = e.positions_of(&ids);
        let s = e.state();
        for (k, id) in ids.iter().enumerate() {
            let expected = s.bodies.index_of(*id).map(|i| s.bodies.positions[i]);
            match (pos[k], expected) {
                (Some(a), Some(b)) => assert!((a - b).length() < 1e-5, "{id} : {a} ≠ {b}"),
                (a, b) => assert_eq!(a, b, "body {id}"),
            }
        }
        let c = e.center_of_mass().unwrap();
        assert!((c - s.bodies.center_of_mass().unwrap()).length() < 1e-4);
        if !report.merges.is_empty() {
            assert_eq!(report.merges, vec![(10, 20)]);
            assert_eq!(pos[0], None);
            merged = true;
        }
    }
    assert!(merged, "the test must cause a merger");
}

/// Device shared with a (simulated) display, `None` without a graphics card.
fn shared_device() -> Option<(stellar_engine_gpu::SharedDevice, wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::from_env()
            .unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL),
        ..Default::default()
    });
    let Some(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("GPU test skipped: no graphics card");
        return None;
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
    .expect("opening the graphics card");
    let shared = stellar_engine_gpu::SharedDevice {
        adapter,
        device: device.clone(),
        queue: queue.clone(),
    };
    Some((shared, device, queue))
}

/// Copies the start of a GPU buffer back to main memory.
fn read_buffer(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer, size: u64) -> Vec<u8> {
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
    raw
}

/// Followed body: once a step has run, its position is written on the GPU at
/// every step, equal to the one the engine publishes; an absent body has
/// w = 0, and a body no longer followed is no longer advertised.
#[test]
fn followed_body_position_is_written_every_step() {
    use stellar_engine_gpu::BodiesView;

    let Some((shared, device, queue)) = shared_device() else { return };
    let mut e = match GpuEngine::with_device(sphere(500, 5), params(), &shared) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("GPU test skipped: {msg}");
            return;
        }
    };
    let view = e
        .live_view()
        .expect("view exposed on a shared device")
        .downcast::<BodiesView>()
        .expect("GPU engine view");
    let id = e.state().bodies.ids[42];
    let read = |buffer: &wgpu::Buffer| {
        let raw = read_buffer(&device, &queue, buffer, 16);
        let f = |k: usize| f32::from_le_bytes(raw[4 * k..4 * k + 4].try_into().unwrap()) as f64;
        (DVec3::new(f(0), f(1), f(2)), f(3))
    };

    view.follow(Some(id));
    assert!(view.followed_position(id).is_none(), "not computed before a step");
    for _ in 0..10 {
        e.step();
        let (pos, found) = read(view.followed_position(id).expect("computed by the step"));
        assert_eq!(found, 1.0);
        assert_eq!(Some(pos), e.positions_of(&[id])[0]);
    }

    let absent = 1_000_000;
    view.follow(Some(absent));
    e.step();
    assert!(view.followed_position(id).is_none(), "another body is followed");
    let (_, found) = read(view.followed_position(absent).expect("computed by the step"));
    assert_eq!(found, 0.0);

    view.follow(None);
    e.step();
    assert!(view.followed_position(absent).is_none());
}

/// Shared device: the buffers exposed to the display contain, at the
/// advertised offsets, exactly the positions and masses of the state.
#[test]
fn shared_device_exposes_current_bodies() {
    use stellar_engine_gpu::BodiesView;

    let Some((shared, device, queue)) = shared_device() else { return };

    // Collapsing cloud, large radii: mergers, hence compactions.
    let ic = InitialConditions {
        count: 2_000,
        radius: 10.0,
        seed: 11,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Rest,
        ..Default::default()
    };
    let mut p = PhysicsParams::suggested(&ic, 1.0);
    p.base_radius *= 20.0;
    let mut e = match GpuEngine::with_device(generate_state(&ic, 1.0), p, &shared) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("GPU test skipped: {msg}");
            return;
        }
    };
    let view = e
        .live_view()
        .expect("view exposed on a shared device")
        .downcast::<BodiesView>()
        .expect("GPU engine view");

    let mut merged = 0;
    for _ in 0..300 {
        merged += e.step().merges.len();
    }
    assert!(merged > 0, "the test must cause mergers");

    let (buffer, count) = view.current();
    assert_eq!(count as usize, e.body_count());
    let raw = read_buffer(&device, &queue, buffer, count as u64 * BodiesView::STRIDE);

    let s = e.state();
    let stride = BodiesView::STRIDE as usize;
    for i in 0..count as usize {
        let f = |off: usize| f32::from_le_bytes(raw[off..off + 4].try_into().unwrap()) as f64;
        let o = i * stride + BodiesView::POSITION_OFFSET as usize;
        let pos = DVec3::new(f(o), f(o + 4), f(o + 8));
        let mass = f(i * stride + BodiesView::MASS_OFFSET as usize);
        assert_eq!(pos, s.bodies.positions[i]);
        assert_eq!(mass, s.bodies.masses[i]);
        assert!(mass > 0.0, "dead body in the published state");
    }

    // Without a shared device: no view.
    let own = GpuEngine::new(sphere(100, 3), params()).expect("carte graphique");
    assert!(own.live_view().is_none());
}

/// Central system, compact ejected cluster (bound to itself, not to the
/// system) and an isolated ejected body.
fn ejection_scene(cluster: u64) -> BodySet {
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
    for k in 0..cluster {
        let a = k as f64 * 2.3;
        b.push(
            1_000 + k,
            DVec3::new(0.0, 30.0, 0.0) + DVec3::new(a.cos(), a.sin(), (a * 0.5).cos()) * 0.2,
            DVec3::new(0.0, 50.0, 0.0),
            1.0,
        );
    }
    b.push(
        200,
        DVec3::new(-30.0, 0.0, 0.0),
        DVec3::new(-50.0, 0.0, 0.0),
        1.0,
    );
    b
}

fn ejection_params() -> PhysicsParams {
    let mut p = params();
    p.softening = 0.01;
    p.cleanup_period = 1;
    p
}

#[test]
fn cleanup_keeps_bound_ejected_cluster() {
    let Some(mut e) = engine(state(ejection_scene(20)), ejection_params()) else {
        return;
    };
    let r = e.step();
    assert_eq!(r.removed, vec![200]);
    assert_eq!(e.body_count(), 70);
    let clusters = r.clusters.expect("clusters described at clean-up");
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].bodies, 20);
    assert!((clusters[0].mass - 20.0).abs() < 1e-4);
    let ids = e.state().bodies.ids;
    assert!(ids.contains(&1_019) && !ids.contains(&200));
    // The moments read back are corrected for the removed body.
    let com = e.center_of_mass().expect("barycentre");
    let expected = e.state().bodies.center_of_mass().expect("barycentre");
    assert!((com - expected).length() < 1e-2, "{com} ≠ {expected}");
}

#[test]
fn cleanup_removes_small_ejected_groups() {
    let Some(mut e) = engine(state(ejection_scene(5)), ejection_params()) else {
        return;
    };
    let r = e.step();
    let mut removed = r.removed.clone();
    removed.sort_unstable();
    assert_eq!(removed, vec![200, 1_000, 1_001, 1_002, 1_003, 1_004]);
    assert_eq!(e.body_count(), 50);
}

/// Tidal field and insertion: the GPU engine follows the CPU engine.
#[test]
fn external_field_and_insertion_match_cpu() {
    use stellar_domain::ExternalField;
    let mut b = BodySet::default();
    for i in 0..200u64 {
        let a = i as f64 * 2.399_963;
        let z = 1.0 - 2.0 * (i as f64 + 0.5) / 200.0;
        let s = (1.0 - z * z).sqrt();
        b.push(i, DVec3::new(a.cos() * s, a.sin() * s, z), DVec3::ZERO, 1.0);
    }
    let mut p = params();
    p.softening = 0.5;
    let Some(mut gpu) = engine(state(b.clone()), p) else {
        return;
    };
    let mut cpu = CpuEngine::new(state(b), p);
    let field = ExternalField::of_point_mass(1.0, 5_000.0, DVec3::new(30.0, 0.0, 0.0), DVec3::ZERO, 0.0);
    let mut extra = BodySet::default();
    extra.push(900, DVec3::new(0.0, 3.0, 0.0), DVec3::new(0.5, 0.0, 0.0), 2.0);
    extra.push(901, DVec3::new(0.0, -3.0, 0.0), DVec3::new(-0.5, 0.0, 0.0), 2.0);
    for k in 0..300 {
        if k == 100 {
            assert!(gpu.insert(&extra) && cpu.insert(&extra));
        }
        assert!(gpu.set_external_field(field) && cpu.set_external_field(field));
        gpu.step();
        cpu.step();
    }
    assert_eq!(gpu.body_count(), 202);
    let (mg, mc) = (gpu.moments(), cpu.moments());
    assert!((mg.mass - mc.mass).abs() < 1e-3);
    // The field accelerates the whole system towards the source (≈ 1.67 in x);
    // the GPU engine's own drift (single precision, grouped traversal) stays
    // of the order of a hundredth.
    assert!(mc.velocity.x > 1.5);
    assert!(
        (mg.velocity - mc.velocity).length() < 3e-2,
        "{} ≠ {}",
        mg.velocity,
        mc.velocity
    );
    assert!((mg.center - mc.center).length() < 1e-2, "{} ≠ {}", mg.center, mc.center);
    let ids = [900, 901, 17];
    for (a, c) in gpu.positions_of(&ids).iter().zip(cpu.positions_of(&ids)) {
        let (a, c) = (a.expect("present"), c.expect("present"));
        assert!((a - c).length() < 5e-2, "{a} ≠ {c}");
    }
}

/// Split of an ejected cluster with the GPU engine.
#[test]
fn composite_split_on_gpu() {
    use stellar_application::MultiSystem;
    use stellar_engine_gpu::GpuEngineFactory;
    if GpuEngineFactory::probe().is_err() {
        return;
    }
    let mut p = ejection_params();
    p.softening = 0.05;
    let s = SimulationState {
        initial_mass: Some(71.0),
        ..state(ejection_scene(20))
    };
    let mut e = MultiSystem::new(std::sync::Arc::new(GpuEngineFactory::new()), s, p)
        .expect("composite engine");
    let r = e.step();
    assert_eq!(r.removed, vec![200]);
    let systems = e.systems();
    assert_eq!(systems.len(), 2);
    assert_eq!(systems[1].bodies, 20);
    for _ in 0..20 {
        e.step();
    }
    let st = e.state();
    assert_eq!(st.systems.len(), 2);
    assert!(st.bodies.ids.contains(&1_019));
}

