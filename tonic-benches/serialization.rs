#[macro_use]
extern crate criterion;

use criterion::black_box;
use criterion::Criterion;

use tonic_benches;
use tonic_benches::just_random;

fn random_gen_baseline(n: usize) {
    tonic_benches::just_random(n);
}

fn load_complied_protobuf(n: usize) {
    tonic_benches::load(n);
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("Build a Tonic Request", |b| {
        b.iter(|| load_complied_protobuf(black_box(20)))
    });
    c.bench_function("Baseline random string gen to factor out", |b| {
        b.iter(|| just_random(black_box(20)))
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
