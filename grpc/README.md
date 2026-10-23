# gRPC-Rust

The official Rust implementation of [gRPC], a high performance, open source,
universal RPC framework.

> NOTE: This version is a preview and not recommended for any production
> use.  All APIs are unstable.  Proceed at your own risk.

## Documentation, Examples, and Getting Started

Please see [our website] for everything you should need to get started using
gRPC!

[our website]: https://grpc.io/docs/languages/rust

### Rust Version

`grpc`'s MSRV is `1.88`.

### Project Layout

- `grpc` (this crate): The core gRPC implementation.
- [`protoc-gen-grpc-rust`]: The protobuf code generator binary, used with [`protoc`].
- [`grpc-protobuf-build`]: Build integration for grpc protobuf code generation.
- [`grpc-protobuf`]: Implementation of protobuf-over-grpc used by the generated code.

## License

This project is licensed under the [MIT license](../LICENSE) and/or the [`Apache
license 2.0`](https://www.apache.org/licenses/LICENSE-2.0.txt).

### Contributing

If you'd like to contribute, please see our project's
[`CONTRIBUTING.md`](../CONTRIBUTING.md) for some guidelines.

[gRPC]: https://grpc.io
[`protoc`]: https://protobuf.dev/installation/
[`protoc-gen-grpc-rust`]: ../protoc-gen-grpc-rust
[`grpc-protobuf-build`]: ../grpc-protobuf-build
[`grpc-protobuf`]: ../grpc-protobuf
