# tonic-build

Provides code generation for service stubs to use with tonic. For protobuf compilation via prost, use the `tonic-prost-build` crate.

# Feature flags

- `cleanup-markdown`: Enables cleaning up documentation from the generated code.
  Useful when documentation of the generated code fails `cargo test --doc` for example.
  The `prost` feature must be enabled to use this feature.
- `prost`: Enables usage of prost generator (enabled by default).
- `transport`: Enables generation of `connect` method using `tonic::transport::Channel`
  (enabled by default).

## Features

Required dependencies

```toml
[dependencies]
tonic = "<tonic-version>"
prost = "<prost-version>"

[build-dependencies]
tonic-prost-build = "<tonic-version>"
```

## Getting Started

For protobuf compilation, use `tonic-prost-build` in your [`build.rs` file](https://doc.rust-lang.org/cargo/reference/build-scripts.html) at the root of the binary/library.

You can rely on the defaults via

```rust,no_run,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::compile_protos("proto/service.proto")?;
    Ok(())
}
```

Or configure the generated code deeper via

```rust,no_run,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
   tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(
            &["proto/helloworld/helloworld.proto"],
            &["proto/helloworld"],
        )?;
   Ok(())
}
```

For further details how to use the generated client/server, see the [examples here](https://github.com/hyperium/tonic/tree/master/examples) or the Google APIs example below.


## NixOS related hints

On NixOS, it is better to specify the location of `PROTOC` and `PROTOC_INCLUDE` explicitly.

```bash
$ export PROTOBUF_LOCATION=$(nix-env -q protobuf --out-path --no-name)
$ export PROTOC=$PROTOBUF_LOCATION/bin/protoc
$ export PROTOC_INCLUDE=$PROTOBUF_LOCATION/include
$ cargo build
```

The reason being that if `prost_build::compile_protos` fails to generate the resultant package,
the failure is not obvious until the `include!(concat!(env!("OUT_DIR"), "/resultant.rs"));`
fails with `No such file or directory` error.

### Google APIs example
A good way to use Google API is probably using git submodules.

So suppose in our `proto` folder we do:
```bash
git submodule add https://github.com/googleapis/googleapis

git submodule update --remote
```

And a bunch of Google proto files in structure will be like this:
```raw
├── googleapis
│   └── google
│       ├── api
│       │   ├── annotations.proto
│       │   ├── client.proto
│       │   ├── field_behavior.proto
│       │   ├── http.proto
│       │   └── resource.proto
│       └── pubsub
│           └── v1
│               ├── pubsub.proto
│               └── schema.proto
```

Then we can generate Rust code via this setup in our `build.rs`:

```rust,no_run,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .build_server(false)
        //.out_dir("src/google")  // you can change the generated code's location
        .compile_protos(
            &["proto/googleapis/google/pubsub/v1/pubsub.proto"],
            &["proto/googleapis"], // specify the root location to search proto dependencies
        )?;
    Ok(())
}
```

Then you can reference the generated Rust like this this in your code:
```rust,ignore
pub mod api {
    tonic::include_proto!("google.pubsub.v1");
}
use api::{publisher_client::PublisherClient, ListTopicsRequest};
```

Or if you want to save the generated code in your own code base,
you can uncomment the line `.out_dir(...)` above, and in your lib file
config a mod like this:
```rust,ignore
pub mod google {
    #[path = ""]
    pub mod pubsub {
        #[path = "google.pubsub.v1.rs"]
        pub mod v1;
    }
}
```
See [the example here](https://github.com/hyperium/tonic/tree/master/examples/src/gcp)
