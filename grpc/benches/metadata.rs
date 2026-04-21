/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use criterion::Criterion;
use criterion::criterion_group;
use criterion::criterion_main;
use grpc::metadata::MetadataKey;
use grpc::metadata::MetadataMap;
use grpc::metadata::MetadataValue;
use std::hint::black_box;
use tonic::metadata::MetadataMap as TonicMetadataMap;

fn bench_metadata_map_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_map_insert");

    for size in [5, 10, 20].iter() {
        group.bench_with_input(format!("grpc_metadata_map_{}", size), size, |b, &size| {
            b.iter(|| {
                let mut map = MetadataMap::with_capacity(size);
                for i in 0..size {
                    let key_str = format!("x-header-{}", i);
                    let key = MetadataKey::from_bytes(key_str.as_bytes()).unwrap();
                    let val = MetadataValue::try_from("value").unwrap();
                    map.insert(key, val);
                }
                black_box(map);
            });
        });

        group.bench_with_input(format!("tonic_metadata_map_{}", size), size, |b, &size| {
            b.iter(|| {
                let mut map = TonicMetadataMap::with_capacity(size);
                for i in 0..size {
                    let key_str = format!("x-header-{}", i);
                    let key = key_str
                        .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                        .unwrap();
                    let val = "value"
                        .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                        .unwrap();
                    map.insert(key, val);
                }
                black_box(map);
            });
        });
    }
    group.finish();
}

fn bench_metadata_map_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_map_append");

    for size in [5, 10, 20].iter() {
        group.bench_with_input(format!("grpc_metadata_map_{}", size), size, |b, &size| {
            b.iter(|| {
                let mut map = MetadataMap::with_capacity(size);
                for i in 0..size {
                    let key_str = format!("x-header-{}", i);
                    let key = MetadataKey::from_bytes(key_str.as_bytes()).unwrap();
                    let val = MetadataValue::try_from("value").unwrap();
                    map.append(key, val);
                }
                black_box(map);
            });
        });

        group.bench_with_input(format!("tonic_metadata_map_{}", size), size, |b, &size| {
            b.iter(|| {
                let mut map = TonicMetadataMap::with_capacity(size);
                for i in 0..size {
                    let key_str = format!("x-header-{}", i);
                    let key = key_str
                        .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                        .unwrap();
                    let val = "value"
                        .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                        .unwrap();
                    map.append(key, val);
                }
                black_box(map);
            });
        });
    }
    group.finish();
}

fn bench_metadata_map_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_map_get");

    for size in [5, 10, 20].iter() {
        let mut map = MetadataMap::with_capacity(*size);
        let mut keys = Vec::new();
        for i in 0..*size {
            let key_str = format!("x-header-{}", i);
            let key = MetadataKey::from_bytes(key_str.as_bytes()).unwrap();
            map.insert(key.clone(), MetadataValue::try_from("value").unwrap());
            keys.push(key);
        }

        group.bench_with_input(format!("grpc_metadata_map_{}", size), size, |b, _| {
            b.iter(|| {
                for key in &keys {
                    black_box(map.get(key));
                }
            });
        });

        let mut tonic_map = TonicMetadataMap::with_capacity(*size);
        let mut tonic_keys = Vec::new();
        for i in 0..*size {
            let key_str = format!("x-header-{}", i);
            let key = key_str
                .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                .unwrap();
            tonic_map.insert(key.clone(), "value".parse().unwrap());
            tonic_keys.push(key);
        }

        group.bench_with_input(format!("tonic_metadata_map_{}", size), size, |b, _| {
            b.iter(|| {
                for key in &tonic_keys {
                    black_box(tonic_map.get(key));
                }
            });
        });
    }
    group.finish();
}

fn bench_metadata_map_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_map_iter");

    for size in [5, 10, 20].iter() {
        let mut map = MetadataMap::with_capacity(*size);
        for i in 0..*size {
            let key_str = format!("x-header-{}", i);
            let key = MetadataKey::from_bytes(key_str.as_bytes()).unwrap();
            map.insert(key, MetadataValue::try_from("value").unwrap());
        }

        group.bench_with_input(format!("grpc_metadata_map_{}", size), size, |b, _| {
            b.iter(|| {
                for entry in map.iter() {
                    black_box(entry);
                }
            });
        });

        let mut tonic_map = TonicMetadataMap::with_capacity(*size);
        for i in 0..*size {
            let key_str = format!("x-header-{}", i);
            let key = key_str
                .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                .unwrap();
            tonic_map.insert(key, "value".parse().unwrap());
        }

        group.bench_with_input(format!("tonic_metadata_map_{}", size), size, |b, _| {
            b.iter(|| {
                for entry in tonic_map.iter() {
                    black_box(entry);
                }
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_metadata_map_insert,
    bench_metadata_map_append,
    bench_metadata_map_get,
    bench_metadata_map_iter,
);
criterion_main!(benches);
