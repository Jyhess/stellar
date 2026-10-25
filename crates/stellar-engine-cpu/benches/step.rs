use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use stellar_application::generator::generate_state;
use stellar_domain::{Distribution, InitialConditions, PhysicsEngine, PhysicsParams, VelocityMode};
use stellar_engine_cpu::CpuEngine;

fn bench_step(c: &mut Criterion) {
    let mut group = c.benchmark_group("pas_de_simulation");
    group.sample_size(10);
    for &n in &[10_000usize, 100_000, 1_000_000] {
        let ic = InitialConditions {
            count: n,
            radius: 100.0,
            seed: 1,
            distribution: Distribution::Sphere,
            velocity: VelocityMode::Rest,
        };
        let params = PhysicsParams::suggested(&ic, 1.0);
        let mut engine = CpuEngine::new(generate_state(&ic, 1.0), params);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| engine.step());
        });
    }
    group.finish();
}

criterion_group!(benches, bench_step);
criterion_main!(benches);
