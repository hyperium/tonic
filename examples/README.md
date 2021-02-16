# Examples

Set of examples that show off the features provided by `tonic`.

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
