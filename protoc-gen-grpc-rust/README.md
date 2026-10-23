# protoc-gen-grpc-rust

A protoc plugin that generates Rust gRPC service code for use with the [`grpc`
crate](https://crates.io/crates/grpc).  This crate is generally not needed
directly; instead most users will use
[`grpc-protobuf-build`](https://crates.io/crates/grpc-protobuf-build), which
depends on this crate.

> NOTE: This version is a preview and not recommended for any production
> use.  All APIs are unstable.  Proceed at your own risk.

Note: as part of compiling, the source files for `protoc` are downloaded from
the [protobuf github repository](https://github.com/protocolbuffers/protobuf).
The archive's checksum is verified before compiling.

## Using from Rust

A `build.rs` script will ensure the `protoc` and `protoc-gen-grpc-rust` binaries
are compiled, and the functions `protoc` and `protoc_gen_grpc_rust` can be used
to find their locations.

### Skipping C++ Compilation

If you want to bypass the C++ compilation step in your application, set the
following environment variable:

```bash
export PROTOC_GEN_GRPC_RUST_NO_BUILD=1
```

When set, the C++ build is skipped, and no binaries will be present in the
output directory.  This will cause `grpc-protobuf-build` to fall back to the
`GRPC_RUST_PROTOC_DIR` environment variable, and then your `PATH` to find the
protoc plugin.

## Building binaries manually

Requirements:
- CMake 3.14 or higher
- C++17 compatible compiler

From the `src/cpp_source` directory:

```bash
# Configure (downloads protobuf and dependencies automatically)
cmake -B build -DCMAKE_BUILD_TYPE=Release

# Build
cmake --build build --config Release --parallel

# Install
cmake --install build --config Release --prefix install
```

The binaries will be in `build/bin/`:
- `protoc` - The protobuf compiler
- `protoc-gen-grpc-rust` - The Rust gRPC code generator plugin

## Usage

**Note:** It's generally recommended to use `grpc_protobuf_build::CodeGen`
and/or `protobuf_codegen::CodeGen` instead of invoking `protoc` directly.

```bash
# Add the plugin to PATH
export PATH="$PWD/build/bin:$PATH"

# Generate Rust gRPC code
protoc \
  --rust_opt="experimental-codegen=enabled,kernel=upb" \
  --rust_out=./generated \
  --grpc-rust_out=./generated \
  your_service.proto
```

## Available Options

* `message_module_path=PATH` (optional): Specifies the Rust path to the module where Protobuf messages are defined.
  * Default: `self`
  * Example: `message_module_path=crate::pb::messages`

* `crate_mapping=PATH` (optional): Specifies the path to a crate mapping file for multi-crate projects.