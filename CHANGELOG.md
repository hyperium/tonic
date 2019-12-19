# [0.1.0-beta.1](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.6...v0.1.0-beta.1) (December 19, 2019)


### Bug Fixes

* **build:** Allow creating multiple services in the same package ([#173](https://github.com/hyperium/tonic/issues/173)) ([0847b67](https://github.com/hyperium/tonic/commit/0847b67))
* **build:** Prevent duplicated client/server generated code ([#121](https://github.com/hyperium/tonic/issues/121)) ([b02b4b2](https://github.com/hyperium/tonic/commit/b02b4b2))
* **build:** Remove async ready ([#185](https://github.com/hyperium/tonic/issues/185)) ([97d5363](https://github.com/hyperium/tonic/commit/97d5363))
* **build:** snake_case service names ([#190](https://github.com/hyperium/tonic/issues/190)) ([3a5c66d](https://github.com/hyperium/tonic/commit/3a5c66d))
* **docs:** typo in lib.rs ([#142](https://github.com/hyperium/tonic/issues/142)) ([c63c107](https://github.com/hyperium/tonic/commit/c63c107))
* **examples:** Remove use of VecDeque as a placeholder type ([#143](https://github.com/hyperium/tonic/issues/143)) ([354d4fd](https://github.com/hyperium/tonic/commit/354d4fd))
* **transport:** Fix infinite recursion in `poll_ready` ([#192](https://github.com/hyperium/tonic/issues/192)) ([c99d13c](https://github.com/hyperium/tonic/commit/c99d13c)), closes [#184](https://github.com/hyperium/tonic/issues/184) [#191](https://github.com/hyperium/tonic/issues/191)
* **transport:** Fix lazily reconnecting ([#187](https://github.com/hyperium/tonic/issues/187)) ([0505dff](https://github.com/hyperium/tonic/commit/0505dff)), closes [#167](https://github.com/hyperium/tonic/issues/167)
* **transport:** Load balance connecting panic ([#128](https://github.com/hyperium/tonic/issues/128)) ([23e7695](https://github.com/hyperium/tonic/commit/23e7695)), closes [#127](https://github.com/hyperium/tonic/issues/127)
* **transport:** Remove support for OpenSSL ([#141](https://github.com/hyperium/tonic/issues/141)) ([8506050](https://github.com/hyperium/tonic/commit/8506050))
* **transport:** Remove with_rustls for tls config ([#188](https://github.com/hyperium/tonic/issues/188)) ([502491a](https://github.com/hyperium/tonic/commit/502491a))
* Sanitize custom metadata ([#138](https://github.com/hyperium/tonic/issues/138)) ([f9502df](https://github.com/hyperium/tonic/commit/f9502df))
* **transport:** Update builders to move self ([#132](https://github.com/hyperium/tonic/issues/132)) ([85ef18f](https://github.com/hyperium/tonic/commit/85ef18f))


### Features

* **transport:** Add `remote_addr` to `Request` on the server si… ([#186](https://github.com/hyperium/tonic/issues/186)) ([3eb76ab](https://github.com/hyperium/tonic/commit/3eb76ab))
* **transport:** Add server graceful shutdown ([#169](https://github.com/hyperium/tonic/issues/169)) ([393a57e](https://github.com/hyperium/tonic/commit/393a57e))
* **transport:** Add system root anchors for TLS ([#114](https://github.com/hyperium/tonic/issues/114)) ([ac0e333](https://github.com/hyperium/tonic/commit/ac0e333)), closes [#101](https://github.com/hyperium/tonic/issues/101)
* **transport:** Add tracing support to server ([#175](https://github.com/hyperium/tonic/issues/175)) ([f46a454](https://github.com/hyperium/tonic/commit/f46a454))
* **transport:** Allow custom IO and UDS example ([#184](https://github.com/hyperium/tonic/issues/184)) ([b90c340](https://github.com/hyperium/tonic/commit/b90c340)), closes [#136](https://github.com/hyperium/tonic/issues/136)
* expose tcp_nodelay for clients and servers ([#145](https://github.com/hyperium/tonic/issues/145)) ([0eb9991](https://github.com/hyperium/tonic/commit/0eb9991))
* **transport:** Enable TCP_NODELAY. ([#120](https://github.com/hyperium/tonic/issues/120)) ([0299509](https://github.com/hyperium/tonic/commit/0299509))
* **transport:** Expose tcp keepalive to clients & servers ([#151](https://github.com/hyperium/tonic/issues/151)) ([caccfad](https://github.com/hyperium/tonic/commit/caccfad))
* Add `Status` constructors ([#137](https://github.com/hyperium/tonic/issues/137)) ([997241c](https://github.com/hyperium/tonic/commit/997241c))


### BREAKING CHANGES

* **build:** Build will now generate each service client and server into their own modules.
* **build:** Remove async ready from generated clients.
* **transport:** Remove tls config usage of `with_rustls` for `new`.
* **transport:** Remove support for OpenSSL within the transport.
* **transport:** Update builders to move self



# [0.1.0-alpha.6](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.5...v0.1.0-alpha.6) (November 10, 2019)

### Bug Fixes

* **build:** Prevent duplicated client/server generated code ([#121](https://github.com/hyperium/tonic/issues/121)) ([b02b4b2](https://github.com/hyperium/tonic/commit/b02b4b2))
* **transport:** Load balance connecting panic ([#128](https://github.com/hyperium/tonic/issues/128)) ([23e7695](https://github.com/hyperium/tonic/commit/23e7695)), closes [#127](https://github.com/hyperium/tonic/issues/127)


### Features

* **transport:** Add system root anchors for TLS ([#114](https://github.com/hyperium/tonic/issues/114)) ([ac0e333](https://github.com/hyperium/tonic/commit/ac0e333)), closes [#101](https://github.com/hyperium/tonic/issues/101)
* **transport:** Enable TCP_NODELAY. ([#120](https://github.com/hyperium/tonic/issues/120)) ([0299509](https://github.com/hyperium/tonic/commit/0299509))

# [0.1.0-alpha.5](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.4...v0.1.0-alpha.5) (October 23, 2019)

### Bug Fixes

* **build:** Fix missing argument in generate_connect ([#95](https://github.com/hyperium/tonic/issues/95)) ([eea3c0f](https://github.com/hyperium/tonic/commit/eea3c0f))
* **codec:** Enforce encoders/decoders are `Sync` ([#84](https://github.com/hyperium/tonic/issues/84)) ([3ce61d9](https://github.com/hyperium/tonic/commit/3ce61d9)), closes [#81](https://github.com/hyperium/tonic/issues/81)
* **codec:** Remove custom content-type  ([#104](https://github.com/hyperium/tonic/issues/104)) ([a17049f](https://github.com/hyperium/tonic/commit/a17049f))


### Features

* **transport:** Add service multiplexing/routing ([#99](https://github.com/hyperium/tonic/issues/99)) ([5b4f468](https://github.com/hyperium/tonic/commit/5b4f468)), closes [#29](https://github.com/hyperium/tonic/issues/29)
* **transport:** Change channel connect to be async ([#107](https://github.com/hyperium/tonic/issues/107)) ([5c2f4db](https://github.com/hyperium/tonic/commit/5c2f4db))
* Add `IntoRequest` and `IntoStreamingRequest` traits ([#66](https://github.com/hyperium/tonic/issues/66)) ([4bb087b](https://github.com/hyperium/tonic/commit/4bb087b))


### BREAKING CHANGES

* **transport:** `Endpoint::channel` was removed in favor of an async `Endpoint::connect`.
* **codec** `Streaming<T>` now requires that the inner stream also implements `Sync`.
* **codec** `Codec` trait no longer requires `CONTENT_TYPE` and now always uses `application/grpc`.

# [0.1.0-alpha.5](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.3...v0.1.0-alpha.5) (October 23, 2019)


### Bug Fixes

* **build:** Fix service and rpc name conflict ([#92](https://github.com/hyperium/tonic/issues/92)) ([1dbde95](https://github.com/hyperium/tonic/commit/1dbde95)), closes [#89](https://github.com/hyperium/tonic/issues/89)
* **codec:** Properly decode partial DATA frames ([#83](https://github.com/hyperium/tonic/issues/83)) ([9079e0f](https://github.com/hyperium/tonic/commit/9079e0f))
* **transport:** Rename server tls config method ([#73](https://github.com/hyperium/tonic/issues/73)) ([2a4bdb2](https://github.com/hyperium/tonic/commit/2a4bdb2))


### Features

* **docs:** Add routeguide tutorial ([#21](https://github.com/hyperium/tonic/issues/21)) ([5d0a795](https://github.com/hyperium/tonic/commit/5d0a795))
* **transport:** Add support client mTLS ([#77](https://github.com/hyperium/tonic/issues/77)) ([335a373](https://github.com/hyperium/tonic/commit/335a373))


### BREAKING CHANGES

* **transport:** `rustls_client_config` for the server has been renamed to `rustls_server_config`.
* **client:** Use `Stream` instead of `TrySteam` for client calls ([#61](https://github.com/hyperium/tonic/issues/61)) ([7eda823](https://github.com/hyperium/tonic/commit/7eda823))


# [0.1.0-alpha.3](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.2...v0.1.0-alpha.3) (October 9, 2019)


### Features

* **build:** Expose prost-build type_attributes and field_attribu… ([#60](https://github.com/hyperium/tonic/issues/60)) ([06ff619](https://github.com/hyperium/tonic/commit/06ff619))
* **transport:** Expose more granular control of TLS configuration ([#48](https://github.com/hyperium/tonic/issues/48)) ([8db3961](https://github.com/hyperium/tonic/commit/8db3961))



# 0.1.0-alpha.2 (October 7, 2019)

### Bug Fixes

* **codec:** Fix buffer decode panic on full ([#43](https://github.com/hyperium/tonic/issues/43)) ([ed3e7e9](https://github.com/hyperium/tonic/commit/ed3e7e9))
* **codegen:** Fix Empty protobuf type and add unimplemented ([#26](https://github.com/hyperium/tonic/issues/26)) ([2670b34](https://github.com/hyperium/tonic/commit/2670b34))
* **codegen:** Use wellknown types from `prost-types` ([#49](https://github.com/hyperium/tonic/issues/49)) ([4e1fcec](https://github.com/hyperium/tonic/commit/4e1fcec))
* **transport:** Attempt to load RSA private keys in rustls ([#39](https://github.com/hyperium/tonic/issues/39)) ([2c5c3a2](https://github.com/hyperium/tonic/commit/2c5c3a2))
* **transport:** Avoid exit after bad TLS handshake ([#51](https://github.com/hyperium/tonic/issues/51)) ([412a0bd](https://github.com/hyperium/tonic/commit/412a0bd))


### Features

* **codgen:** Add default implementations for the generated server ([#27](https://github.com/hyperium/tonic/issues/27)) ([4559613](https://github.com/hyperium/tonic/commit/4559613))
* **transport:** Expose HTTP/2 settings ([#28](https://github.com/hyperium/tonic/issues/28)) ([0218d58](https://github.com/hyperium/tonic/commit/0218d58))

# 0.1.0-alpha.1 (October 1, 2019)

- Initial release
