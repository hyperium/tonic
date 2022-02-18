# tonic-build

Compiles proto files via prost and generates service stubs and proto definitiones for use with tonic.

## Features

Required dependencies

```toml
[dependencies]
tonic = <tonic-version>
prost = <prost-version>

[build-dependencies]
tonic-build = <tonic-version>
```

## Examples

### Simple

In `build.rs`:
```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/service.proto")?;
    Ok(())
}
```

### Configuration

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
   tonic_build::configure()
        .build_server(false)
        .compile(
            &["proto/helloworld/helloworld.proto"],
            &["proto/helloworld"],
        )?;
   Ok(())
}
```
See [more examples here](https://github.com/hyperium/tonic/tree/master/examples)

### Google APIs example
A good way to use Google API is probably using git submodules.

So suppose in our `proto` folder we do:
```
git submodule add https://github.com/googleapis/googleapis

git submodule update --remote
```

And a bunch of Google proto files in structure will be like this:
```
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

Then we can generate Rust code via this setup in our `build.rs`
```rust
fn main() {
    tonic_build::configure()
        .build_server(false)
        //.out_dir("src/google")  // you can change the generated code's location
        .compile(
            &["proto/googleapis/google/pubsub/v1/pubsub.proto"],
            &["proto/googleapis"], // specify the root location to search proto dependencies
        ).unwrap();
}
```

Then you can reference the generated Rust like this this in your code:
```rust
pub mod api {
    tonic::include_proto!("google.pubsub.v1");
}
use api::{publisher_client::PublisherClient, ListTopicsRequest};
```

Or if you want to save the generated code in your own code base,
you can uncomment the line `.out_dir(...)` above, and in your lib file
config a mod like this:
```rust
pub mod google {
    #[path = ""]
    pub mod pubsub {
        #[path = "google.pubsub.v1.rs"]
        pub mod v1;
    }
}
```
See [the example here](https://github.com/hyperium/tonic/tree/master/examples/src/gcp)
