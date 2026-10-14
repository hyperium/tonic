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

//! Compares [`CallAttributes`] against [`http::Extensions`] at 5, 10 and 15
//! stored attributes.

use std::hint::black_box;
use std::time::Duration;

use criterion::BenchmarkGroup;
use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::criterion_group;
use criterion::criterion_main;
use criterion::measurement::WallTime;
use grpc::call_attributes::CallAttributes;
use http::Extensions;

/// Shorthand for the criterion group type threaded through the helpers below.
type Group<'a> = BenchmarkGroup<'a, WallTime>;

// -----------------------------------------------------------------------------
// Payload types
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RequestMeta {
    pub id: u64,
    pub flags: u32,
    pub priority: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionContext {
    pub ip: [u8; 4],
    pub port: u16,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadSummary {
    pub bytes_read: usize,
    pub status_code: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub attempts: u32,
    pub keep_alive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u8,
    pub backoff_ms: u32,
}

// -----------------------------------------------------------------------------
// Contender abstraction
// -----------------------------------------------------------------------------

/// The bounds every payload satisfies.
///
/// `Sync` is here only because [`http::Extensions`] demands it;
/// [`CallAttributes`] does not. Requiring it of both keeps one workload able to
/// drive either, at no cost, since all the payloads below are plain data.
trait Attr: Clone + Send + Sync + 'static {}

impl<T: Clone + Send + Sync + 'static> Attr for T {}

/// The slice of behaviour the two contenders share.
///
/// They have no common trait of their own, and their inherent signatures differ
/// (`Extensions::insert` returns the displaced value). This adapter lets one
/// generic workload drive both, so neither gets a subtly different one.
trait TypeMap {
    fn create() -> Self;
    fn put<T: Attr>(&mut self, val: T);
    fn fetch<T: Attr>(&self) -> Option<&T>;
}

impl TypeMap for CallAttributes {
    #[inline(always)]
    fn create() -> Self {
        Self::new()
    }

    #[inline(always)]
    fn put<T: Attr>(&mut self, val: T) {
        self.add(val);
    }

    #[inline(always)]
    fn fetch<T: Attr>(&self) -> Option<&T> {
        self.get::<T>()
    }
}

impl TypeMap for Extensions {
    #[inline(always)]
    fn create() -> Self {
        Self::new()
    }

    #[inline(always)]
    fn put<T: Attr>(&mut self, val: T) {
        self.insert(val);
    }

    #[inline(always)]
    fn fetch<T: Attr>(&self) -> Option<&T> {
        self.get::<T>()
    }
}

// -----------------------------------------------------------------------------
// Workloads
// -----------------------------------------------------------------------------
//
// Each tier is a prefix of a single interleaved list, so every tier holds a
// comparable mix of integers and structs and only the count varies.
//
// `N` is a const parameter rather than an argument so the tier cutoffs are
// folded away at monomorphization: `fill::<M, 5>` contains only its own five
// inserts, with no branch left behind.

/// Inserts the first `N` payloads.
#[inline(always)]
fn fill<M: TypeMap, const N: usize>(map: &mut M) {
    map.put(black_box(42u32));
    map.put(black_box(UserId(12345)));
    map.put(black_box(100_000u64));
    map.put(black_box(RequestMeta {
        id: 1,
        flags: 7,
        priority: 2,
    }));
    map.put(black_box(1024usize));
    if N == 5 {
        return;
    }

    map.put(black_box(Point3D {
        x: 1.0,
        y: 2.0,
        z: 3.0,
    }));
    map.put(black_box(-10i32));
    map.put(black_box(SessionContext {
        ip: [127, 0, 0, 1],
        port: 8080,
        active: true,
    }));
    map.put(black_box(-999_999i64));
    map.put(black_box(PayloadSummary {
        bytes_read: 2048,
        status_code: 200,
    }));
    if N == 10 {
        return;
    }

    map.put(black_box(7u16));
    map.put(black_box(ConnectionInfo {
        attempts: 3,
        keep_alive: true,
    }));
    map.put(black_box(-3i16));
    map.put(black_box(RetryPolicy {
        max_retries: 5,
        backoff_ms: 250,
    }));
    map.put(black_box(9u8));
}

/// Reads back each of the first `N` payload types.
#[inline(always)]
fn probe<M: TypeMap, const N: usize>(map: &M) {
    black_box(map.fetch::<u32>());
    black_box(map.fetch::<UserId>());
    black_box(map.fetch::<u64>());
    black_box(map.fetch::<RequestMeta>());
    black_box(map.fetch::<usize>());
    if N == 5 {
        return;
    }

    black_box(map.fetch::<Point3D>());
    black_box(map.fetch::<i32>());
    black_box(map.fetch::<SessionContext>());
    black_box(map.fetch::<i64>());
    black_box(map.fetch::<PayloadSummary>());
    if N == 10 {
        return;
    }

    black_box(map.fetch::<u16>());
    black_box(map.fetch::<ConnectionInfo>());
    black_box(map.fetch::<i16>());
    black_box(map.fetch::<RetryPolicy>());
    black_box(map.fetch::<u8>());
}

// -----------------------------------------------------------------------------
// Benchmarks
// -----------------------------------------------------------------------------

/// Full lifecycle: build a map of `N` entries, then tear it down.
fn lifecycle_one<M: TypeMap, const N: usize>(group: &mut Group<'_>, label: &str) {
    group.bench_function(BenchmarkId::new(label, N), |b| {
        b.iter(|| {
            let mut map = M::create();
            fill::<M, N>(&mut map);
            black_box(&map);
            drop(map);
        });
    });
}

/// One `fetch::<T>()` for every stored type, against a pre-built map.
fn lookup_one<M: TypeMap, const N: usize>(group: &mut Group<'_>, label: &str) {
    let mut map = M::create();
    fill::<M, N>(&mut map);
    group.bench_function(BenchmarkId::new(label, N), |b| {
        b.iter(|| probe::<M, N>(&map));
    });
}

fn lifecycle_tier<const N: usize>(group: &mut Group<'_>) {
    lifecycle_one::<CallAttributes, N>(group, "CallAttributes");
    lifecycle_one::<Extensions, N>(group, "http::Extensions");
}

fn lookup_tier<const N: usize>(group: &mut Group<'_>) {
    lookup_one::<CallAttributes, N>(group, "CallAttributes");
    lookup_one::<Extensions, N>(group, "http::Extensions");
}

fn bench_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("lifecycle");
    lifecycle_tier::<5>(&mut group);
    lifecycle_tier::<10>(&mut group);
    lifecycle_tier::<15>(&mut group);
    group.finish();
}

fn bench_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("lookup");
    lookup_tier::<5>(&mut group);
    lookup_tier::<10>(&mut group);
    lookup_tier::<15>(&mut group);
    group.finish();
}

fn custom_criterion() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(50)
}

criterion_group! {
    name = benches;
    config = custom_criterion();
    targets = bench_lifecycle, bench_lookup
}
criterion_main!(benches);
