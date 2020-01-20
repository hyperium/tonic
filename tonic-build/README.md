# tonic-build

Compiles proto files via prost and generates service stubs and proto definitiones for use with tonic.

## Features

- rustfmt: This feature enables the use of rustfmt to format the output code this makes the code readable and the error messages nice. This requires that rustfmt is installed. This is enabled by default.

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
