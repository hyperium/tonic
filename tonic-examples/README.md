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

## Load balance

### Client

```bash
$ cargo run --bin load-balance-client
```

### Server

```bash
$ cargo run --bin load-balance-server
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


### Notes:

If you are using the `codegen` feature, then the following dependencies are
**required**:

* [bytes](https://crates.io/crates/bytes)
* [prost](https://crates.io/crates/prost)
* [prost-derive](https://crates.io/crates/prost-derive)

