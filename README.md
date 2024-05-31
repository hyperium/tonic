![](https://github.com/hyperium/tonic/raw/master/.github/assets/tonic-banner.svg?sanitize=true)

A rust implementation of [gRPC], a high performance, open source, general
RPC framework that puts mobile and HTTP/2 first.

[`tonic`] is a gRPC over HTTP/2 implementation focused on high performance, interoperability, and flexibility. This library was created to have first class support of async/await and to act as a core building block for production systems written in Rust.

[![Crates.io](https://img.shields.io/crates/v/tonic)](https://crates.io/crates/tonic)
[![Documentation](https://docs.rs/tonic/badge.svg)](https://docs.rs/tonic)
[![Crates.io](https://img.shields.io/crates/l/tonic)](LICENSE)


[Examples] | [Website] | [Docs] | [Chat]

## Overview

[`tonic`] is composed of three main components: the generic gRPC implementation, the high performance HTTP/2
implementation and the codegen powered by [`prost`]. The generic implementation can support any HTTP/2
implementation and any encoding via a set of generic traits. The HTTP/2 implementation is based on [`hyper`],
a fast HTTP/1.1 and HTTP/2 client and server built on top of the robust [`tokio`] stack. The codegen
contains the tools to build clients and servers from [`protobuf`] definitions.

## Features

- Bi-directional streaming
- High performance async io
- Interoperability
- TLS backed by [`rustls`]
- Load balancing
- Custom metadata
- Authentication
- Health Checking

## Getting Started

Examples can be found in [`examples`] and for more complex scenarios [`interop`]
may be a good resource as it shows examples of many of the gRPC features.

If you're using [rust-analyzer] we recommend you set `"rust-analyzer.cargo.buildScripts.enable": true` to correctly load
the generated code.

For IntelliJ IDEA users, please refer to [this](https://github.com/intellij-rust/intellij-rust/pull/8056) and enable
`org.rust.cargo.evaluate.build.scripts`
[experimental feature](https://plugins.jetbrains.com/plugin/8182-rust/docs/rust-faq.html#experimental-features).

### Rust Version

`tonic`'s MSRV is `1.70`.

```bash
$ rustup update
$ cargo build
```

### Dependencies

In order to build `tonic` >= 0.8.0, you need the `protoc` Protocol Buffers compiler, along with Protocol Buffers resource files.

#### Ubuntu

```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y protobuf-compiler libprotobuf-dev
```

#### Alpine Linux

```sh
sudo apk add protoc protobuf-dev
```

#### macOS

Assuming [Homebrew](https://brew.sh/) is already installed. (If not, see instructions for installing Homebrew on [the Homebrew website](https://brew.sh/).)

```zsh
brew install protobuf
```

#### Windows

- Download the latest version of `protoc-xx.y-win64.zip` from [HERE](https://github.com/protocolbuffers/protobuf/releases/latest)
- Extract the file `bin\protoc.exe` and put it somewhere in the `PATH`
- Verify installation by opening a command prompt and enter `protoc --version`

### Tutorials

- The [`helloworld`][helloworld-tutorial] tutorial provides a basic example of using `tonic`, perfect for first time users!
- The [`routeguide`][routeguide-tutorial] tutorial provides a complete example of using `tonic` and all its
features.

## Getting Help

First, see if the answer to your question can be found in the API documentation.
If the answer is not there, there is an active community in
the [Tonic Discord channel][chat]. We would be happy to try to answer your
question. If that doesn't work, try opening an [issue] with the question.

[chat]: https://discord.gg/6yGkFeN
[issue]: https://github.com/hyperium/tonic/issues/new

## Project Layout

- [`tonic`](https://github.com/hyperium/tonic/tree/master/tonic): Generic gRPC and HTTP/2 client/server
implementation.
- [`tonic-build`](https://github.com/hyperium/tonic/tree/master/tonic-build): [`prost`] based service codegen.
- [`tonic-types`](https://github.com/hyperium/tonic/tree/master/tonic-types): [`prost`] based grpc utility types
  including support for gRPC Well Known Types.
- [`tonic-health`](https://github.com/hyperium/tonic/tree/master/tonic-health): Implementation of the standard [gRPC
health checking service][healthcheck]. Also serves as an example of both unary and response streaming.
- [`tonic-reflection`](https://github.com/hyperium/tonic/tree/master/tonic-reflection): A tonic based gRPC
reflection implementation.
- [`examples`](https://github.com/hyperium/tonic/tree/master/examples): Example gRPC implementations showing off
tls, load balancing and bi-directional streaming.
- [`interop`](https://github.com/hyperium/tonic/tree/master/interop): Interop tests implementation.

## Contributing

:balloon: Thanks for your help improving the project! We are so happy to have
you! We have a [contributing guide][guide] to help you get involved in the Tonic
project.

[guide]: CONTRIBUTING.md

## License

This project is licensed under the [MIT license](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Tonic by you, shall be licensed as MIT, without any additional
terms or conditions.


[gRPC]: https://grpc.io
[`tonic`]: https://github.com/hyperium/tonic
[`tokio`]: https://github.com/tokio-rs/tokio
[`hyper`]: https://github.com/hyperium/hyper
[`prost`]: https://github.com/tokio-rs/prost
[`protobuf`]: https://developers.google.com/protocol-buffers
[`rustls`]: https://github.com/rustls/rustls
[`examples`]: https://github.com/hyperium/tonic/tree/master/examples
[`interop`]: https://github.com/hyperium/tonic/tree/master/interop
[Examples]: https://github.com/hyperium/tonic/tree/master/examples
[Website]: https://github.com/hyperium/tonic
[Docs]: https://docs.rs/tonic
[Chat]: https://discord.gg/6yGkFeN
[routeguide-tutorial]: https://github.com/hyperium/tonic/blob/master/examples/routeguide-tutorial.md
[helloworld-tutorial]: https://github.com/hyperium/tonic/blob/master/examples/helloworld-tutorial.md
[healthcheck]: https://github.com/grpc/grpc/blob/master/doc/health-checking.md
[rust-analyzer]: https://rust-analyzer.github.io
