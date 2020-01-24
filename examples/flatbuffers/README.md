# Examples

Set of examples that show off the features provided by `tonic` and flatbuffers using `butte`.

## Helloworld

### Client

```bash
$ cargo run --bin helloworld-client
```

### Server

```bash
$ cargo run --bin helloworld-server
```

### Notes:

If you are using the `codegen` feature, then the following dependencies are
**required**:

* [bytes](https://crates.io/crates/bytes)
* [butte](https://crates.io/crates/butte) (WIP)
* [butte-build](https://crates.io/crates/butte-build) (WIP)

