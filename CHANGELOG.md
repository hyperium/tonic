# [0.3.0](https://github.com/hyperium/tonic/compare/v0.2.1...v0.3.0) (2020-07-13)

* Bump `base64` to `0.12` and `tokio-rustls`/`rustls-native-certs` to `0.14`/`0.4`. ([#397](https://github.com/hyperium/tonic/pull/397) ([570c606](https://github.com/hyperium/tonic/commit/570c606397e47406ec148fe1763586e87a8f5298))).

### Bug Fixes

* `Status::details` leaking base64 encoding ([#395](https://github.com/hyperium/tonic/issues/395)) ([2c4c544](https://github.com/hyperium/tonic/commit/2c4c544d902c588fc0654910fba1f0d21d78eab3)), closes [#379](https://github.com/hyperium/tonic/issues/379)
* **build:** Allow empty packages ([#382](https://github.com/hyperium/tonic/issues/382)) ([f085aba](https://github.com/hyperium/tonic/commit/f085aba302001986fd04219d2843f659f73c4031)), closes [#381](https://github.com/hyperium/tonic/issues/381)
* **build:** Make generated server service public ([#347](https://github.com/hyperium/tonic/issues/347)) ([8cd6f05](https://github.com/hyperium/tonic/commit/8cd6f0506429cfbe59e63b0216f208482d12358a))
* **transport:** Propagate errors in tls_config instead of unwrap/panic ([#385](https://github.com/hyperium/tonic/issues/385)) ([3b9d6a6](https://github.com/hyperium/tonic/commit/3b9d6a6262b62f30b8c9953f0da8e403be53216e))
* Remove uses of pin_project::project attribute ([#367](https://github.com/hyperium/tonic/issues/367)) ([5bda615](https://github.com/hyperium/tonic/commit/5bda6156328bd2c94bc274588871b666f1b72d6e))


### Features

* **codec:** Improve compression flag log ([#374](https://github.com/hyperium/tonic/issues/374)) ([d68dd36](https://github.com/hyperium/tonic/commit/d68dd365321764aceaf4e37a106a519797926495))
* **transport:** Add Endpoint::connect_lazy method ([#392](https://github.com/hyperium/tonic/issues/392)) ([ec9046d](https://github.com/hyperium/tonic/commit/ec9046dfc23d63828363d9555cd7b96811ad442d)), closes [#167](https://github.com/hyperium/tonic/issues/167)
* **transport:** Add optional service methods ([#275](https://github.com/hyperium/tonic/issues/275)) ([2b997b0](https://github.com/hyperium/tonic/commit/2b997b0c5f37d69f3cd8b5b566b64df110d9f4eb))
* **transport:** Dynamic load balancing ([#341](https://github.com/hyperium/tonic/issues/341)) ([85ae0a4](https://github.com/hyperium/tonic/commit/85ae0a4733b9e99edaa05e65160d98f21f288fc1))
* **types:** Add `tonic-types` crate ([#391](https://github.com/hyperium/tonic/issues/391)) ([ea7fe66](https://github.com/hyperium/tonic/commit/ea7fe66b145e01891f1c1f16d247e02524d98fae))
* Add `Display` implementation for `Code` ([#386](https://github.com/hyperium/tonic/issues/386)) ([ab1de44](https://github.com/hyperium/tonic/commit/ab1de44771f3fa6ac283485bdbf1035d6407ac1a))
* Add `Status::to_http` ([#376](https://github.com/hyperium/tonic/issues/376)) ([327b4ff](https://github.com/hyperium/tonic/commit/327b4fffa3381345ee4620df7e9998efe2aa9454))
* Add metadata to error responses ([#348](https://github.com/hyperium/tonic/issues/348)) ([372da52](https://github.com/hyperium/tonic/commit/372da52e96114ca76cc221f3c598be82bfae970c))
* Add new method get_uri for Endpoint ([#371](https://github.com/hyperium/tonic/issues/371)) ([54d7a7a](https://github.com/hyperium/tonic/commit/54d7a7af6b6530b80353c5741586c38cca8382c9))

### BREAKING CHANGES

* Bumped rustls to `0.18`.
* Endpoint `tls_config` now returns a result.



# [0.2.1](https://github.com/hyperium/tonic/compare/v0.2.0...v0.2.1) (2020-05-07)


### Bug Fixes

* base64 encode details header ([#345](https://github.com/hyperium/tonic/issues/345)) ([e683ffe](https://github.com/hyperium/tonic/commit/e683ffef1fcbe0ace9cc696232489f5f6600e83f))
* **build:** Remove ambiguity in service method call ([#327](https://github.com/hyperium/tonic/issues/327)) ([5d56daa](https://github.com/hyperium/tonic/commit/5d56daa721cfb18edc74cf50db4270e2c8461fc9))
* **transport:** Apply tls-connector for discovery when applicable ([#334](https://github.com/hyperium/tonic/issues/334)) ([#338](https://github.com/hyperium/tonic/issues/338)) ([99fbe22](https://github.com/hyperium/tonic/commit/99fbe22e7c1340d6be9ee5d3ae9738850881af61))


### Features

* **transport:** Add AsRef impl for Certificate ([#326](https://github.com/hyperium/tonic/issues/326)) ([d2ad8df](https://github.com/hyperium/tonic/commit/d2ad8df629a349cc151a0a4ede96f04356f73839))


# [0.2.0](https://github.com/hyperium/tonic/compare/v0.1.1...v0.2.0) (2020-04-01)


### Bug Fixes

* **build:** Allow non_camel_case_types on codegen structs ([224280d](https://github.com/hyperium/tonic/commit/224280dfff8944e9e553337416d23d6e5a050945)), closes [#295](https://github.com/hyperium/tonic/issues/295)
* **build:** Don't replace extern_paths ([#261](https://github.com/hyperium/tonic/issues/261)) ([1b3d107](https://github.com/hyperium/tonic/commit/1b3d107206136312a2536d3b72748c52191d99b1))
* **build:** Ignore non `.rs` files with rustfmt ([#284](https://github.com/hyperium/tonic/issues/284)) ([7dfa2a2](https://github.com/hyperium/tonic/commit/7dfa2a277b593e008cea53eef7163ca59a06c56a)), closes [#283](https://github.com/hyperium/tonic/issues/283)
* **build:** Implement Debug for client struct ([6dbe88d](https://github.com/hyperium/tonic/commit/6dbe88d445e378fff48d05083c23baeb2020cb2d)), closes [#298](https://github.com/hyperium/tonic/issues/298)
* **build:** Remove debug println! ([#287](https://github.com/hyperium/tonic/issues/287)) ([e2c2be2](https://github.com/hyperium/tonic/commit/e2c2be2f084b7c1ef4e93f6994cb9c728de0c1ed))
* **build:** Server service uses generic body bound ([#306](https://github.com/hyperium/tonic/issues/306)) ([5758b75](https://github.com/hyperium/tonic/commit/5758b758b2d44059b0149a31542d11589999a789))
* **health:** Set referenced version of tonic ([59c7788](https://github.com/hyperium/tonic/commit/59c77888464a0302993dbe07fed7c1848b415f8f))
* **metadata:** Remove deprecated error description ([61e0429](https://github.com/hyperium/tonic/commit/61e0429ae810354363835c36a046b5113b3c74b4))
* **transport:** Handle tls accepting on task ([#320](https://github.com/hyperium/tonic/issues/320)) ([04a8c0c](https://github.com/hyperium/tonic/commit/04a8c0c82a4007f48c3bf3539a3f2312746fedd1))


### Features

* **build:** Add support for custom prost config ([#318](https://github.com/hyperium/tonic/issues/318)) ([202093c](https://github.com/hyperium/tonic/commit/202093c31715b52997c6c206c758924ff5f69bc8))
* **health:** Add tonic-health server impl ([da92dbf](https://github.com/hyperium/tonic/commit/da92dbf8aa885ea0ea05755e9432532fc980e353)), closes [#135](https://github.com/hyperium/tonic/issues/135) [#135](https://github.com/hyperium/tonic/issues/135)
* Add Status with Details Constructor ([#308](https://github.com/hyperium/tonic/issues/308)) ([cfd59db](https://github.com/hyperium/tonic/commit/cfd59dbb342a8b7d216f4856e13d24b564c606f3))
* **build:** Decouple codgen from `prost` ([#170](https://github.com/hyperium/tonic/issues/170)) ([f65cda1](https://github.com/hyperium/tonic/commit/f65cda1ea0a190fe07c4f8d91473baad9a6f1f77))
* **transport:** Expose http2 keep-alive support ([#307](https://github.com/hyperium/tonic/issues/307)) ([012fa3c](https://github.com/hyperium/tonic/commit/012fa3cb4a0e010dafa28305416fab6c4278fc7b))


### BREAKING CHANGES

* Bumped `rustls` to `0.17`.
* Removed deprecated `Error::description` implementations.


# [0.1.1](https://github.com/hyperium/tonic/compare/v0.1.0...v0.1.1) (2020-01-20)


### Bug Fixes

* **build:** Typo with client mod docstring ([#237](https://github.com/hyperium/tonic/issues/237)) ([5fc6762](https://github.com/hyperium/tonic/commit/5fc6762435494d8df023bea8e35a5d20d81f2f3b))
* **transport:** Add Connected impl for TcpStream ([#245](https://github.com/hyperium/tonic/issues/245)) ([cfdf0af](https://github.com/hyperium/tonic/commit/cfdf0aff549196af0c3b7f6e531dbeacfb6990dc))
* **transport:** Use Uri host if no domain for tls ([#244](https://github.com/hyperium/tonic/issues/244)) ([6de0b4d](https://github.com/hyperium/tonic/commit/6de0b4d26fd82b4d1303080b0ba8c4db2d4f0fd1))



# [0.1.0](https://github.com/hyperium/tonic/compare/v0.1.0-beta.1...v0.1.0) (2020-01-14)


### Bug Fixes

* **build:** Remove default impl for Server traits ([#229](https://github.com/hyperium/tonic/issues/229)) ([a41f55a](https://github.com/hyperium/tonic/commit/a41f55ab9dfe77fca920b3c2e89343c7ce963225))
* **transport:** Improve `Error` type ([#217](https://github.com/hyperium/tonic/issues/217)) ([ec1f37e](https://github.com/hyperium/tonic/commit/ec1f37e4b46279d20f4fadafa5bf30cfb729fa42))


### chore

* rename ServiceName -> NamedService ([#233](https://github.com/hyperium/tonic/issues/233)) ([6ee2ed9](https://github.com/hyperium/tonic/commit/6ee2ed9b4ff30c0517d70908c6348a633dab5b91))


### Features

* Add gRPC interceptors ([#232](https://github.com/hyperium/tonic/issues/232)) ([eba7ec7](https://github.com/hyperium/tonic/commit/eba7ec7b32fb96938cbdc3d2dfd91c238afda0dc))
* **build:** Add extern_path config support ([#223](https://github.com/hyperium/tonic/issues/223)) ([e034288](https://github.com/hyperium/tonic/commit/e034288c3739467238aee54fdbe0a2a3a87bf824))
* **codec:** Introduce `Decoder/Encoder` traits ([#208](https://github.com/hyperium/tonic/issues/208)) ([0fa2bf1](https://github.com/hyperium/tonic/commit/0fa2bf1cea9d1166d49e40f2211268611b6993de))
* **transport:** Add `serve_with_incoming_shutdown` ([#220](https://github.com/hyperium/tonic/issues/220)) ([a66595b](https://github.com/hyperium/tonic/commit/a66595bfe3c146daaa437bddd5ce3db4542b1bf6))
* **transport:** Add server side peer cert support ([#228](https://github.com/hyperium/tonic/issues/228)) ([af807c3](https://github.com/hyperium/tonic/commit/af807c3ccd283cee0e424e75298cd176424767ca))


### BREAKING CHANGES

* Rename `ServiceName` to `NamedService`.
* removed `interceptor_fn` and `intercep_headers_fn` from `transport` in favor of using `tonic::Interceptor`.
* **codec:** Add new `Decoder/Encoder` traits and use `EncodeBuf/DecodeBuf` over `BytesMut` directly.
* **build:** remove default implementations for server traits.



# [0.1.0-beta.1](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.5...v0.1.0-beta.1) (2019-12-19)


### Bug Fixes

* **build:** Allow creating multiple services in the same package ([#173](https://github.com/hyperium/tonic/issues/173)) ([0847b67](https://github.com/hyperium/tonic/commit/0847b67c4eb66a814c8c447a57fade2552e64a85))
* **build:** Prevent duplicated client/server generated code ([#121](https://github.com/hyperium/tonic/issues/121)) ([b02b4b2](https://github.com/hyperium/tonic/commit/b02b4b238bfee96b886609396b957e2592477ecb))
* **build:** Remove async ready ([#185](https://github.com/hyperium/tonic/issues/185)) ([97d5363](https://github.com/hyperium/tonic/commit/97d5363e2b2aee456edc5db4b5b53316c8b40745))
* **build:** snake_case service names ([#190](https://github.com/hyperium/tonic/issues/190)) ([3a5c66d](https://github.com/hyperium/tonic/commit/3a5c66d5f236eaece05dbd9fd1e1a00a3ab98259))
* **docs:** typo in lib.rs ([#142](https://github.com/hyperium/tonic/issues/142)) ([c63c107](https://github.com/hyperium/tonic/commit/c63c107560db165303c369487006b3507a0e7e07))
* **examples:** Remove use of VecDeque as a placeholder type ([#143](https://github.com/hyperium/tonic/issues/143)) ([354d4fd](https://github.com/hyperium/tonic/commit/354d4fdc35dc51575f4c685fc04354f2058061ff))
* **transport:** Fix infinite recursion in `poll_ready` ([#192](https://github.com/hyperium/tonic/issues/192)) ([c99d13c](https://github.com/hyperium/tonic/commit/c99d13c6e669be3a6ecf428ae32d4b937393738a)), closes [#184](https://github.com/hyperium/tonic/issues/184) [#191](https://github.com/hyperium/tonic/issues/191)
* **transport:** Fix lazily reconnecting ([#187](https://github.com/hyperium/tonic/issues/187)) ([0505dff](https://github.com/hyperium/tonic/commit/0505dff65a18c162c3ae398d42ed20ac54351439)), closes [#167](https://github.com/hyperium/tonic/issues/167)
* **transport:** Load balance connecting panic ([#128](https://github.com/hyperium/tonic/issues/128)) ([23e7695](https://github.com/hyperium/tonic/commit/23e7695800d8f22ee8e0ba7456f5ffc4b19430c3)), closes [#127](https://github.com/hyperium/tonic/issues/127)
* **transport:** Remove support for OpenSSL ([#141](https://github.com/hyperium/tonic/issues/141)) ([8506050](https://github.com/hyperium/tonic/commit/85060500f3a8f91ed47c632e07896c9e5567629a))
* **transport:** Remove with_rustls for tls config ([#188](https://github.com/hyperium/tonic/issues/188)) ([502491a](https://github.com/hyperium/tonic/commit/502491a59031dc0aa6e51a764f8edab04ab85581))
* Sanitize custom metadata ([#138](https://github.com/hyperium/tonic/issues/138)) ([f9502df](https://github.com/hyperium/tonic/commit/f9502dfd7ef306fff86c83b711bc96623555ef5c))
* **transport:** Update builders to move self ([#132](https://github.com/hyperium/tonic/issues/132)) ([85ef18f](https://github.com/hyperium/tonic/commit/85ef18f8b7f91047ca5bcfe5fc90e3c510c7936a))


### Features

* **transport:** Add `remote_addr` to `Request` on the server si… ([#186](https://github.com/hyperium/tonic/issues/186)) ([3eb76ab](https://github.com/hyperium/tonic/commit/3eb76abf9fdce5f903de1a7f05b8afc8694fa0ce))
* **transport:** Add server graceful shutdown ([#169](https://github.com/hyperium/tonic/issues/169)) ([393a57e](https://github.com/hyperium/tonic/commit/393a57eadebb8e2e6d3633f70141edba647b5f65))
* **transport:** Add system root anchors for TLS ([#114](https://github.com/hyperium/tonic/issues/114)) ([ac0e333](https://github.com/hyperium/tonic/commit/ac0e333b39f60f9c304d7798a49e07e9f08a16d4)), closes [#101](https://github.com/hyperium/tonic/issues/101)
* **transport:** Add tracing support to server ([#175](https://github.com/hyperium/tonic/issues/175)) ([f46a454](https://github.com/hyperium/tonic/commit/f46a45401d42f6c8b6ab449f7462735a9aea0bfc))
* **transport:** Allow custom IO and UDS example ([#184](https://github.com/hyperium/tonic/issues/184)) ([b90c340](https://github.com/hyperium/tonic/commit/b90c3408001f762a32409f7e2cf688ebae39d89e)), closes [#136](https://github.com/hyperium/tonic/issues/136)
* expose tcp_nodelay for clients and servers ([#145](https://github.com/hyperium/tonic/issues/145)) ([0eb9991](https://github.com/hyperium/tonic/commit/0eb9991b9fcd4a688904788966d1e5ab74918571))
* **transport:** Enable TCP_NODELAY. ([#120](https://github.com/hyperium/tonic/issues/120)) ([0299509](https://github.com/hyperium/tonic/commit/029950904a5e1398bb508446b660c1863e9f631c))
* **transport:** Expose tcp keepalive to clients & servers ([#151](https://github.com/hyperium/tonic/issues/151)) ([caccfad](https://github.com/hyperium/tonic/commit/caccfad7e7b03d42aa1679c00a270c92a621bb0f))
* Add `Status` constructors ([#137](https://github.com/hyperium/tonic/issues/137)) ([997241c](https://github.com/hyperium/tonic/commit/997241c43fdb390caad19a41dc6bf67724de521a))


### BREAKING CHANGES

* **build:** Build will now generate each service client and server into their own modules.
* **transport:** Remove support for OpenSSL within the transport.



# [0.1.0-alpha.5](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.4...v0.1.0-alpha.5) (2019-10-31)


### Bug Fixes

* **build:** Fix missing argument in generate_connect ([#95](https://github.com/hyperium/tonic/issues/95)) ([eea3c0f](https://github.com/hyperium/tonic/commit/eea3c0f99ac292efb7b8d4956fa014108af871ac))
* **codec:** Enforce encoders/decoders are `Sync` ([#84](https://github.com/hyperium/tonic/issues/84)) ([3ce61d9](https://github.com/hyperium/tonic/commit/3ce61d9860528dd4a13f719774d5c649198fb55c)), closes [#81](https://github.com/hyperium/tonic/issues/81)
* **codec:** Remove custom content-type  ([#104](https://github.com/hyperium/tonic/issues/104)) ([a17049f](https://github.com/hyperium/tonic/commit/a17049f1f72c9655a72fef8021072d56b3f4e543))


### Features

* **transport:** Add service multiplexing/routing ([#99](https://github.com/hyperium/tonic/issues/99)) ([5b4f468](https://github.com/hyperium/tonic/commit/5b4f4689a253ccca34f34bb5329b420efb9159c1)), closes [#29](https://github.com/hyperium/tonic/issues/29)
* **transport:** Change channel connect to be async ([#107](https://github.com/hyperium/tonic/issues/107)) ([5c2f4db](https://github.com/hyperium/tonic/commit/5c2f4dba322b28e8132b21acfa184309de791d12))
* Add `IntoRequest` and `IntoStreamingRequest` traits ([#66](https://github.com/hyperium/tonic/issues/66)) ([4bb087b](https://github.com/hyperium/tonic/commit/4bb087b5ff19636a20e10a669ba3b46f99c84358))


### BREAKING CHANGES

* **transport:** `Endpoint::channel` was removed in favor of
an async `Endpoint::connect`.



# [0.1.0-alpha.4](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.3...v0.1.0-alpha.4) (2019-10-23)


### Bug Fixes

* **build:** Fix service and rpc name conflict ([#92](https://github.com/hyperium/tonic/issues/92)) ([1dbde95](https://github.com/hyperium/tonic/commit/1dbde95d844378121af54f16d9f8aa9f0f7fc2f2)), closes [#89](https://github.com/hyperium/tonic/issues/89)
* **client:** Use `Stream` instead of `TrySteam` for client calls ([#61](https://github.com/hyperium/tonic/issues/61)) ([7eda823](https://github.com/hyperium/tonic/commit/7eda823c9cbe6054c39b42f8f3e7efce4698aebe))
* **codec:** Properly decode partial DATA frames ([#83](https://github.com/hyperium/tonic/issues/83)) ([9079e0f](https://github.com/hyperium/tonic/commit/9079e0f66bc75d2ce49a5537bf66c9ff5effbdab))
* **transport:** Rename server tls config method ([#73](https://github.com/hyperium/tonic/issues/73)) ([2a4bdb2](https://github.com/hyperium/tonic/commit/2a4bdb24f62bb3bbceb73e9551ba70512f94c187))


### Features

* **docs:** Add routeguide tutorial ([#21](https://github.com/hyperium/tonic/issues/21)) ([5d0a795](https://github.com/hyperium/tonic/commit/5d0a7955541509d2dbfdb9b689fb57cd2b842172))
* **transport:** Add support client mTLS ([#77](https://github.com/hyperium/tonic/issues/77)) ([335a373](https://github.com/hyperium/tonic/commit/335a373a403615a9737b2e19d0089c89bcaa3c4e))


### BREAKING CHANGES

* **transport:** `rustls_client_config` for the server has been renamed to `rustls_server_config`.



# [0.1.0-alpha.3](https://github.com/hyperium/tonic/compare/v0.1.0-alpha.2...v0.1.0-alpha.3) (2019-10-09)


### Features

* **build:** Expose prost-build type_attributes and field_attribu… ([#60](https://github.com/hyperium/tonic/issues/60)) ([06ff619](https://github.com/hyperium/tonic/commit/06ff619944a2f44d3aea60e653b39157c392f541))
* **transport:** Expose more granular control of TLS configuration ([#48](https://github.com/hyperium/tonic/issues/48)) ([8db3961](https://github.com/hyperium/tonic/commit/8db3961491c35955c76bf2da6a17bf8a60e3b146))



# [0.1.0-alpha.2](https://github.com/hyperium/tonic/compare/2670b349f96666c8d30d9d5d6ac2e611bb4584e2...v0.1.0-alpha.2) (2019-10-08)


### Bug Fixes

* **codec:** Fix buffer decode panic on full ([#43](https://github.com/hyperium/tonic/issues/43)) ([ed3e7e9](https://github.com/hyperium/tonic/commit/ed3e7e95a5401b9b224640e17908c2182286197d))
* **codegen:** Fix Empty protobuf type and add unimplemented ([#26](https://github.com/hyperium/tonic/issues/26)) ([2670b34](https://github.com/hyperium/tonic/commit/2670b349f96666c8d30d9d5d6ac2e611bb4584e2))
* **codegen:** Use wellknown types from `prost-types` ([#49](https://github.com/hyperium/tonic/issues/49)) ([4e1fcec](https://github.com/hyperium/tonic/commit/4e1fcece150fb1f373b0ccbb69d302463ed6bcfd))
* **transport:** Attempt to load RSA private keys in rustls ([#39](https://github.com/hyperium/tonic/issues/39)) ([2c5c3a2](https://github.com/hyperium/tonic/commit/2c5c3a282a1ccc9288bc0f6fb138fc123f45dd09))
* **transport:** Avoid exit after bad TLS handshake ([#51](https://github.com/hyperium/tonic/issues/51)) ([412a0bd](https://github.com/hyperium/tonic/commit/412a0bd697b4822b94c55cb18d2373a6ed75b690))


### Features

* **codgen:** Add default implementations for the generated serve… ([#27](https://github.com/hyperium/tonic/issues/27)) ([4559613](https://github.com/hyperium/tonic/commit/4559613c37f75dde67981ee38a7f5af5947ef0be))
* **transport:** Expose http/2 settings ([#28](https://github.com/hyperium/tonic/issues/28)) ([0218d58](https://github.com/hyperium/tonic/commit/0218d58c282d6de6f300229677c99369d3ea20ed))



