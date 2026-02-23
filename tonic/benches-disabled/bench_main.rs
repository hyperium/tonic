#![cfg(feature = "broken")]

use criterion::*;

mod benchmarks;

criterion_group!(
    benches,
    benchmarks::request_response::bench_throughput,
    benchmarks::request_response_diverse_types::bench_throughput,
);
criterion_main!(benches);
