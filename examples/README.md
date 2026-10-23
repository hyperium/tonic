# Examples

Set of examples that show off the features provided by `tonic` and `grpc`.

In order to build these examples, you must have the `protoc` Protocol Buffers
compiler.  You need to have installed either:

 * the `protoc` binary, made available in your PATH. This is the default.

 * A compatible C++ compiler and CMake as described in the
   [`protoc-gen-grpc-rust` crate](../protoc-gen-grpc-rust/README.md).  Choose
   this option by passing `--features protoc-gen-grpc-rust` to `cargo`.

If you choose to install protobuf, here are the steps for a variety of operating
systems.

Ubuntu:

```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y protobuf-compiler libprotobuf-dev
```

Alpine Linux:

```sh
sudo apk add protoc protobuf-dev
```

macOS:

Assuming [Homebrew](https://brew.sh/) is already installed. (If not, see instructions for installing Homebrew on [the Homebrew website](https://brew.sh/).)

```zsh
brew install protobuf
```

# `grpc` crate examples

For the examples related to the `grpc` crate, the generated code is checked into
the repo to allow building without `protoc`.  To rebuild the generated code you
must set `GRPC_RUST_REGENERATE_PROTO=1` in your environment.  This requires that
you have installed either:

 * the `protoc` and `protoc-gen-grpc-rust` binaries, made available in your
   PATH.  See above for `protoc`.  `protoc-gen-grpc-rust` can be downloaded from
   [our releases].

 * A compatible C++ compiler and CMake as described in the
   [`protoc-gen-grpc-rust` crate](../protoc-gen-grpc-rust/README.md).  Choose
   this option by passing `--features grpc-protobuf-build/build-plugin` to `cargo`.

[our releases]: https://github.com/grpc/grpc-rust/releases

## Helloworld

### Client

```bash
$ cargo run --bin grpc-helloworld-client
```

### Server

The `grpc` crate currently does not support servers; run the Tonic helloworld
server instead:

```bash
$ cargo run --bin helloworld-server
```

## RouteGuide

### Client

```bash
$ cargo run --bin grpc-routeguide-client
```

### Server

The `grpc` crate currently does not support servers; run the Tonic routeguide
server instead:

```bash
$ cargo run --bin routeguide-server
```

## Google Cloud Pub/Sub Example
This example demonstrates fetching a list of topics from the Cloud Pub/Sub API. 
The request is secured using an OAuth token and TLS.

### Client

Ensure your environment has [Application Default Credentials] configured.
You can do this by setting the `GOOGLE_APPLICATION_CREDENTIALS` environment
variable, or by running the `gcloud auth application-default login` command.

Once your credentials are set up, you will need your GCP Project ID, which can
be found on the main dashboard of the Google Cloud Console. With both of these
ready, you can run the example like so:
```bash
$ cargo run --bin grpc-gcp-client --features grpc-gcp -- <project-id>
```

[Application Default Credentials]: https://docs.cloud.google.com/docs/authentication/application-default-credentials

## Helloworld

### Client

```bash
$ cargo run --bin helloworld-client
```

### Server

```bash
$ cargo run --bin helloworld-server
```

## RouteGuide

### Client

```bash
$ cargo run --bin routeguide-client
```

### Server

```bash
$ cargo run --bin routeguide-server
```

## Authentication

### Client

```bash
$ cargo run --bin authentication-client
```

### Server

```bash
$ cargo run --bin authentication-server
```

## Load Balance

### Client

```bash
$ cargo run --bin load-balance-client
```

### Server

```bash
$ cargo run --bin load-balance-server
```

## Dynamic Load Balance

### Client

```bash
$ cargo run --bin dynamic-load-balance-client
```

### Server

```bash
$ cargo run --bin dynamic-load-balance-server
```

## TLS (rustls)

### Client

```bash
$ cargo run --bin tls-client
```

### Server

```bash
$ cargo run --bin tls-server
```

## Health Checking

### Server

```bash
$ cargo run --bin health-server
```

## Server Reflection

### Server
```bash
$ cargo run --bin reflection-server
```

## Tower Middleware

### Server

```bash
$ cargo run --bin tower-server
```

## Autoreloading Server

### Server
```bash
systemfd --no-pid -s http::[::1]:50051 -- cargo watch -x 'run --bin autoreload-server'
```

### Notes:

If you are using the `codegen` feature, then the following dependencies are
**required**:

* [bytes](https://crates.io/crates/bytes)
* [prost](https://crates.io/crates/prost)
* [prost-derive](https://crates.io/crates/prost-derive)

The autoload example requires the following crates installed globally:

* [systemfd](https://crates.io/crates/systemfd)
* [cargo-watch](https://crates.io/crates/cargo-watch)

## Richer Error

Both clients and both servers do the same thing, but using the two different
approaches. Run one of the servers in one terminal, and then run the clients
in another.

### Client using the `ErrorDetails` struct

```bash
$ cargo run --bin richer-error-client
```

### Client using a vector of error message types

```bash
$ cargo run --bin richer-error-client-vec
```

### Server using the `ErrorDetails` struct

```bash
$ cargo run --bin richer-error-server
```

### Server using a vector of error message types

```bash
$ cargo run --bin richer-error-server-vec
```
