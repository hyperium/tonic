# tonic-protobuf-build

Compiles proto files via protobuf rust and generates service stubs and proto
definitions for use with tonic.

## Features

Required dependencies

```toml
[dependencies]
tonic = "<tonic-version>"
protobuf = "<protobuf-version>"
tonic-protobuf =  "<tonic-version>"

[build-dependencies]
tonic-protobuf-build = "<tonic-version>"
```

You must ensure you have the following programs in your PATH:
1. protoc
1. protoc-gen-rust-grpc

## Getting Started

`tonic-protobuf-build` works by being included as a [`build.rs` file](https://doc.rust-lang.org/cargo/reference/build-scripts.html) at the root of the binary/library.

You can rely on the defaults via

```rust,no_run
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_protobuf_build::CodeGen::new()
        .include("proto")
        .inputs(["service.proto"])
        .compile()?;
    Ok(())
}
```

Or configure the generated code deeper via

```rust,no_run
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dependency = tonic_protobuf_build::Dependency::builder()
        .crate_name("external_protos".to_string())
        .proto_import_paths(vec![PathBuf::from("external/message.proto")])
        .proto_files(vec!["message.proto".to_string()])
        .build()?;

    tonic_protobuf_build::CodeGen::new()
        .generate_message_code(false)
        .inputs(["proto/helloworld/helloworld.proto"])
        .include("external")
        .message_module_path("super::proto")
        .dependencies(vec![dependency])
        //.out_dir("src/generated")  // you can change the generated code's location
        .compile()?;
   Ok(())
}
```

Then you can reference the generated Rust like this in your code:
```rust,ignore
mod protos {
    // Include message code.
    include!(concat!(env!("OUT_DIR"), "proto/helloworld/generated.rs"));
}

mod grpc {
    // Include service code.
    include!(concat!(env!("OUT_DIR"), "proto/helloworld/helloworld_grpc.pb.rs"));
}
```

If you don't modify the `message_module_path`, you can use the `include_proto`
macro to simplify the import code.
```rust,ignore
pub mod grpc_pb {
    grpc::include_proto!("proto/helloworld", "helloworld");
}
```

Or if you want to save the generated code in your own code base,
you can uncomment the line `.output_dir(...)` above, and in your lib file
config a mod like this:
```rust,ignore
pub mod generated {
    pub mod helloworld {
        pub mod proto {
            include!("helloworld/generated.rs");
        }

        pub mod grpc {
            include!("helloworld/test_grpc.pb.rs");
        }
    }
}
```
