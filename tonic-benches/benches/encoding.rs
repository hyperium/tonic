#[macro_use]
extern crate criterion;

use criterion::black_box;
use criterion::Criterion;

fn basic_encoding(n: usize) {}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("Basic Encoding", |b| {
        b.iter(|| basic_encoding(black_box(20)))
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
