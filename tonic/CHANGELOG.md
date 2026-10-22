# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.14.7](https://github.com/grpc/grpc-rust/compare/tonic-v0.14.6...tonic-v0.14.7) - 2026-10-22

### Fixed

- *(codec)* eliminate uninitialized memory in encode_item on unwind ([#2847](https://github.com/grpc/grpc-rust/pull/2847))
- *(transport)* drop listener before draining connections ([#2824](https://github.com/grpc/grpc-rust/pull/2824))
- fix/silence all clippy warnings ([#2807](https://github.com/grpc/grpc-rust/pull/2807))
- *(status)* map h2 FRAME_SIZE_ERROR to Code::Internal ([#2641](https://github.com/grpc/grpc-rust/pull/2641))
- *(codec)* respect server's enabled encodings when selecting response compression ([#2655](https://github.com/grpc/grpc-rust/pull/2655))

### Other

- fix changelog duplicate and update repository URLs ([#2943](https://github.com/grpc/grpc-rust/pull/2943))
- *(deps)* require patched rustls-webpki and webpki-roots ([#2940](https://github.com/grpc/grpc-rust/pull/2940))
- handle ipv6 endpoints when tls is enabled ([#2423](https://github.com/grpc/grpc-rust/pull/2423))
- support immediate cancellation of streams without sending End-of-Stream ([#2791](https://github.com/grpc/grpc-rust/pull/2791))
- wrap connection_timeout in Fuse to prevent panic ([#2780](https://github.com/grpc/grpc-rust/pull/2780))
- License update ([#2749](https://github.com/grpc/grpc-rust/pull/2749))

## [0.14.6](https://github.com/hyperium/tonic/compare/tonic-v0.14.5...tonic-v0.14.6) - 2026-05-06

### Added

- *(transport/channel)* expose ServerCertVerifier API ([#2612](https://github.com/hyperium/tonic/pull/2612))

### Fixed

- map no trailers ok status to unknown ([#2543](https://github.com/hyperium/tonic/pull/2543))

### Other

- add max_frame_size to client Endpoint ([#2592](https://github.com/hyperium/tonic/pull/2592))
- Allow setting the HTTP/2 client header table size ([#2582](https://github.com/hyperium/tonic/pull/2582))
- update rust edition and version to 2024 and 1.88, respectively ([#2525](https://github.com/hyperium/tonic/pull/2525))
