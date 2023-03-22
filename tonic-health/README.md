# tonic-health

A `tonic` based gRPC healthcheck implementation. It closely follows the official [health checking protocol](https://github.com/grpc/grpc/blob/master/doc/health-checking.md), although it may not implement all features described in the specs.

Please follow the example in the [main repo](https://github.com/hyperium/tonic/tree/master/examples/src/health) to see how it works.

## Features

- transport: Provides the ability to set the service by using the type system and the
`NamedService` trait. You can use it like that:
```rust
    let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
    let client = HealthClient::new(conn);
```
