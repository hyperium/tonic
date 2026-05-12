# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.14.6](https://github.com/hyperium/tonic/compare/tonic-v0.14.5...tonic-v0.14.6) - 2026-05-06

### Added

- *(transport/channel)* expose ServerCertVerifier API ([#2612](https://github.com/hyperium/tonic/pull/2612))

### Fixed

- map no trailers ok status to unknown ([#2543](https://github.com/hyperium/tonic/pull/2543))

### Other

- add max_frame_size to client Endpoint ([#2592](https://github.com/hyperium/tonic/pull/2592))
- Allow setting the HTTP/2 client header table size ([#2582](https://github.com/hyperium/tonic/pull/2582))
- update rust edition and version to 2024 and 1.88, respectively ([#2525](https://github.com/hyperium/tonic/pull/2525))
