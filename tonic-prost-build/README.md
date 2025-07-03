# tonic-prost-build

Prost build integration for [tonic] gRPC framework.

## Overview

This crate provides code generation for gRPC services using protobuf definitions via the [prost] ecosystem. It bridges [prost-build] with [tonic]'s generic code generation infrastructure.

## Usage

Add to your `build.rs`:

```rust
fn main() {
    tonic_prost_build::configure()
        .compile_protos(&["proto/service.proto"], &["proto"])
        .unwrap();
}
```

## Features

- `prost`: Enables prost-based protobuf code generation (enabled by default)
- `transport`: Enables transport layer code generation
- `cleanup-markdown`: Enables markdown cleanup in generated documentation

[tonic]: https://github.com/hyperium/tonic
[prost]: https://github.com/tokio-rs/prost
[prost-build]: https://github.com/tokio-rs/prost