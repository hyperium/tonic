# Examples

Set of examples that show off the features provided by `tonic`.

In order to build these examples, you must have the `protoc` Protocol Buffers compiler
installed, along with the Protocol Buffers resource files.

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