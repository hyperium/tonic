# protoc-gen-rust-grpc

A protoc plugin that generates Rust gRPC service code using the Tonic framework.

## Build

Requirements:
- CMake 3.14 or higher
- C++17 compatible compiler

```bash
# Create build directory
mkdir build && cd build

# Configure (downloads protobuf and dependencies automatically)
cmake .. -DCMAKE_BUILD_TYPE=Release

# Build
cmake --build . --parallel

# Optional: specify a different protobuf version
cmake .. -DCMAKE_BUILD_TYPE=Release -DPROTOBUF_VERSION=28.3
```

The binaries will be in `build/bin/`:
- `protoc` - The protobuf compiler
- `protoc-gen-rust-grpc` - The Rust gRPC code generator plugin

## Usage

**Note:** It's generally recommended to use `tonic_protobuf_build::CodeGen` and/or `protobuf_codegen::CodeGen` instead of invoking `protoc` directly.

```bash
# Add the plugin to PATH
export PATH="$PWD/build/bin:$PATH"

# Generate Rust gRPC code
protoc \
  --rust_opt="experimental-codegen=enabled,kernel=upb" \
  --rust_out=./generated \
  --rust-grpc_out=./generated \
  your_service.proto
```

## Available Options

* `message_module_path=PATH` (optional): Specifies the Rust path to the module where Protobuf messages are defined.
  * Default: `self`
  * Example: `message_module_path=crate::pb::messages`

* `crate_mapping=PATH` (optional): Specifies the path to a crate mapping file for multi-crate projects.