## Usage example
```sh
# Build the plugin with Bazel
bazel build //src:protoc-gen-rust-grpc

# Set the plugin path
PLUGIN_PATH="$(pwd)/bazel-bin/src/protoc-gen-rust-grpc"

# Run protoc with the Rust and gRPC plugins
protoc \
  --plugin=protoc-gen-grpc-rust="$PLUGIN_PATH" \
  --rust_opt="experimental-codegen=enabled,kernel=upb" \
  --rust_out=./tmp \
  --rust-grpc_opt="experimental-codegen=enabled" \
  --rust-grpc_out=./tmp \
  routeguide.proto
```

## Build
```sh
bazel build //src:protoc-gen-rust-grpc
```

## Language Server Support for development
Generate compile_commands.json using bazel plugin. Configure the language
server to use the generate json file.
```sh
bazel run @hedron_compile_commands//:refresh_all
```
