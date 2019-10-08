#[macro_use]
extern crate criterion;

use criterion::*;
mod utils;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::HelloRequest;

fn build_request(_name: String) {
    let _request = tonic::Request::new(HelloRequest { name: _name });
}

fn bench(c: &mut Criterion) {
    let tiny_string = &utils::generate_rnd_string(2).unwrap();
    let short_string = &utils::generate_rnd_string(20).unwrap();
    let medium_string = &utils::generate_rnd_string(200).unwrap();
    let big_string = &utils::generate_rnd_string(2000).unwrap();
    let huge_string = &utils::generate_rnd_string(20000000).unwrap();

    let mut group = c.benchmark_group("build_request");

    group.throughput(Throughput::Bytes(tiny_string.len() as u64));
    group.bench_function("build_request", |b| {
        b.iter(|| build_request(tiny_string.to_string()))
    });

    group.throughput(Throughput::Bytes(short_string.len() as u64));
    group.bench_function("build_request", |b| {
        b.iter(|| build_request(short_string.to_string()))
    });

    group.throughput(Throughput::Bytes(medium_string.len() as u64));
    group.bench_function("build_request", |b| {
        b.iter(|| build_request(medium_string.to_string()))
    });

    group.throughput(Throughput::Bytes(big_string.len() as u64));
    group.bench_function("build_request", |b| {
        b.iter(|| build_request(big_string.to_string()))
    });

    group.throughput(Throughput::Bytes(huge_string.len() as u64));
    group.bench_function("build_request", |b| {
        b.iter(|| build_request(huge_string.to_string()))
    });

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
