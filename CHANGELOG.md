# [0.1.0-alpha.4](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.3...v0.1.0-alpha.4) (October 23, 2019)


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
