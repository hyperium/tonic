/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */
//! End-to-end tests: a real xDS channel resolving through a fake ADS control
//! plane to live gRPC backends.
//!
//! Unlike the unit tests in [`super::channel`], which inject endpoints through
//! `build_grpc_channel_from_parts`, these tests exercise the full pipeline:
//! bootstrap loading, the ADS transport, and LDS -> RDS -> CDS -> EDS resolution
//! served by [`XdsTestControlPlaneService`]. Traffic is routed to greeter (echo)
//! backends spawned by the tests.
mod test {
    use std::collections::HashMap;
    use std::net::SocketAddr;

    use tokio::time::Duration;
    use xds_test_util::RunningControlPlane;
    use xds_test_util::XdsTestControlPlaneService;
    use xds_test_util::config;

    use crate::BootstrapConfig;
    use crate::XdsChannelBuilder;
    use crate::XdsChannelConfig;
    use crate::XdsChannelGrpc;
    use crate::XdsUri;
    use crate::testutil::grpc::GreeterClient;
    use crate::testutil::grpc::HelloRequest;
    use crate::testutil::grpc::spawn_greeter_server;

    /// Starts the fake ADS control plane on an ephemeral port. Returns the
    /// running control plane (which injects config and shuts down on drop)
    /// and its address.
    async fn start_control_plane() -> (RunningControlPlane, SocketAddr) {
        let running = XdsTestControlPlaneService::new()
            .start()
            .await
            .expect("start control plane");
        let addr = running.addr();
        (running, addr)
    }

    /// Builds a real xDS channel whose bootstrap points at `cp_addr` and whose
    /// target resolves the listener `listener_name`.
    fn build_channel(cp_addr: SocketAddr, listener_name: &str) -> XdsChannelGrpc {
        let bootstrap_json = format!(
            r#"{{"xds_servers":[{{"server_uri":"http://{cp_addr}","channel_creds":[{{"type":"insecure"}}]}}],"node":{{"id":"test"}}}}"#
        );
        let bootstrap = BootstrapConfig::from_json(&bootstrap_json).expect("parse bootstrap");
        let target = XdsUri::parse(&format!("xds:///{listener_name}")).expect("parse target");
        XdsChannelBuilder::new(XdsChannelConfig::new(target).with_bootstrap(bootstrap))
            .build_grpc_channel()
            .expect("build xds channel")
    }

    /// Sends `say_hello` in a loop until a reply starting with `want_prefix` is
    /// observed (xDS resolution and config updates are asynchronous), returning
    /// that reply. Panics if it never arrives.
    async fn say_hello_until_prefix(
        client: &mut GreeterClient<XdsChannelGrpc>,
        want_prefix: &str,
    ) -> String {
        const RETRIES: usize = 100;
        let mut last = None;
        for _ in 0..RETRIES {
            match client
                .say_hello(HelloRequest {
                    name: "world".to_string(),
                })
                .await
            {
                Ok(response) => {
                    let message = response.into_inner().message;
                    if message.starts_with(want_prefix) {
                        return message;
                    }
                    last = Some(Ok(message));
                }
                Err(status) => last = Some(Err(status)),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("never observed reply starting with {want_prefix:?}; last seen: {last:?}");
    }

    /// End-to-end test: verifies that an xDS channel routes traffic to the
    /// correct backend.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn xds_channel_e2e_routes_to_backend() {
        let backend = spawn_greeter_server("backend", None, None)
            .await
            .expect("spawn greeter backend");
        let backend_addr = backend.addr;

        let (control_plane, cp_addr) = start_control_plane().await;

        // Configure LDS (inline route) -> CDS -> EDS pointing at the backend.
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Lds,
            HashMap::from([(
                "my-service".to_string(),
                config::build_inline_listener("my-service", "my-cluster"),
            )]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Cds,
            HashMap::from([(
                "my-cluster".to_string(),
                config::build_cluster("my-cluster"),
            )]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Eds,
            HashMap::from([(
                "my-cluster".to_string(),
                config::build_cla(
                    "my-cluster",
                    &[(backend_addr.ip().to_string(), backend_addr.port())],
                ),
            )]),
        );

        let mut client = GreeterClient::new(build_channel(cp_addr, "my-service"));
        let reply = say_hello_until_prefix(&mut client, "backend:").await;
        assert_eq!(reply, "backend: world");

        // The control plane observed exactly one ADS stream from the client.
        let counts = control_plane.get_service().get_subscriber_counts();
        assert_eq!(counts.get(&config::AdsTypeUrl::Lds), Some(&1));
        assert_eq!(counts.get(&config::AdsTypeUrl::Cds), Some(&1));
        assert_eq!(counts.get(&config::AdsTypeUrl::Eds), Some(&1));

        let _ = backend.shutdown.send(());
    }

    /// The client is initially routing to
    /// one cluster, update the RDS route to point at a different cluster and
    /// assert traffic shifts to the new backend — exercising the control
    /// plane pushing a live update to a connected client.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn xds_channel_e2e_route_update_shifts_traffic() {
        let backend_a = spawn_greeter_server("backend-a", None, None)
            .await
            .expect("spawn backend-a");
        let backend_b = spawn_greeter_server("backend-b", None, None)
            .await
            .expect("spawn backend-b");

        let (control_plane, cp_addr) = start_control_plane().await;

        // LDS -> RDS "route-config"; both clusters and their endpoints are configured
        // up front, and the route initially targets cluster-a.
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Lds,
            HashMap::from([(
                "my-service".to_string(),
                config::build_rds_listener("my-service", "route-config"),
            )]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Rds,
            HashMap::from([(
                "route-config".to_string(),
                config::build_route_config("route-config", "cluster-a"),
            )]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Cds,
            HashMap::from([
                ("cluster-a".to_string(), config::build_cluster("cluster-a")),
                ("cluster-b".to_string(), config::build_cluster("cluster-b")),
            ]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Eds,
            HashMap::from([
                (
                    "cluster-a".to_string(),
                    config::build_cla(
                        "cluster-a",
                        &[(backend_a.addr.ip().to_string(), backend_a.addr.port())],
                    ),
                ),
                (
                    "cluster-b".to_string(),
                    config::build_cla(
                        "cluster-b",
                        &[(backend_b.addr.ip().to_string(), backend_b.addr.port())],
                    ),
                ),
            ]),
        );

        let mut client = GreeterClient::new(build_channel(cp_addr, "my-service"));

        // Initially routed to cluster-a -> backend-a.
        let reply = say_hello_until_prefix(&mut client, "backend-a:").await;
        assert_eq!(reply, "backend-a: world");

        // Update the RDS route to target cluster-b; the control plane pushes the new
        // RouteConfiguration to the connected client.
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Rds,
            HashMap::from([(
                "route-config".to_string(),
                config::build_route_config("route-config", "cluster-b"),
            )]),
        );

        // Traffic shifts to cluster-b -> backend-b.
        let reply = say_hello_until_prefix(&mut client, "backend-b:").await;
        assert_eq!(reply, "backend-b: world");

        let _ = backend_a.shutdown.send(());
        let _ = backend_b.shutdown.send(());
    }

    /// P2C load balancing: with several EDS endpoints behind one cluster, traffic
    /// should spread across all of them. tonic-xds uses power-of-two-choices as
    /// its balancer (see the unit-level `test_xds_channel_grpc_with_p2c_lb`);
    /// this is the full-pipeline version through the control plane.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn xds_channel_e2e_p2c_spreads_across_backends() {
        const NUM_BACKENDS: usize = 5;
        const NUM_REQUESTS: usize = 1000;

        // Spawn several greeter backends, each reporting a distinct name.
        let mut backends = Vec::new();
        for i in 0..NUM_BACKENDS {
            backends.push(
                spawn_greeter_server(&format!("backend-{i}"), None, None)
                    .await
                    .expect("spawn backend"),
            );
        }
        let endpoints: Vec<(String, u16)> = backends
            .iter()
            .map(|backend| (backend.addr.ip().to_string(), backend.addr.port()))
            .collect();

        let (control_plane, cp_addr) = start_control_plane().await;

        // Configure LDS (inline route) -> CDS -> EDS with all backends in one cluster.
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Lds,
            HashMap::from([(
                "my-service".to_string(),
                config::build_inline_listener("my-service", "my-cluster"),
            )]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Cds,
            HashMap::from([(
                "my-cluster".to_string(),
                config::build_cluster("my-cluster"),
            )]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Eds,
            HashMap::from([(
                "my-cluster".to_string(),
                config::build_cla("my-cluster", &endpoints),
            )]),
        );

        let mut client = GreeterClient::new(build_channel(cp_addr, "my-service"));

        // Warm up until xDS resolution completes and the first RPC succeeds.
        say_hello_until_prefix(&mut client, "backend-").await;

        // Tally which backend served each request.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for _ in 0..NUM_REQUESTS {
            let reply = client
                .say_hello(HelloRequest {
                    name: "world".to_string(),
                })
                .await
                .expect("rpc succeeds")
                .into_inner()
                .message;
            let server = reply.split(':').next().unwrap_or_default().to_string();
            *counts.entry(server).or_default() += 1;
        }

        // Every backend should have received a fair share of the traffic.
        assert_eq!(counts.values().sum::<usize>(), NUM_REQUESTS);
        // Each backend should receive at least 50% of the fair share of requests.
        let min_per_backend = (NUM_REQUESTS / NUM_BACKENDS) / 2;
        for i in 0..NUM_BACKENDS {
            let name = format!("backend-{i}");
            let count = counts.get(&name).copied().unwrap_or(0);
            assert!(
                count >= min_per_backend,
                "backend {name} received {count} requests (< {min_per_backend}); distribution: {counts:?}",
            );
        }

        for backend in backends {
            let _ = backend.shutdown.send(());
        }
    }

    /// A CDS response past tonic's 4 MiB decode default only arrives when the
    /// channel raises the limit, so successful routing proves the configured
    /// ceiling reached the ADS transport.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn configured_decoding_limit_admits_oversized_ads_response() {
        const PADDING: usize = 5 * 1024 * 1024;

        let backend = spawn_greeter_server("backend", None, None)
            .await
            .expect("spawn greeter backend");
        let backend_addr = backend.addr;

        let (control_plane, cp_addr) = start_control_plane().await;

        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Lds,
            HashMap::from([(
                "my-service".to_string(),
                config::build_inline_listener("my-service", "my-cluster"),
            )]),
        );
        // `alt_stat_name` is ignored by CDS validation, so it pads the response
        // without changing how the cluster behaves.
        let mut cluster = config::build_cluster("my-cluster");
        cluster.alt_stat_name = "x".repeat(PADDING);
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Cds,
            HashMap::from([("my-cluster".to_string(), cluster)]),
        );
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Eds,
            HashMap::from([(
                "my-cluster".to_string(),
                config::build_cla(
                    "my-cluster",
                    &[(backend_addr.ip().to_string(), backend_addr.port())],
                ),
            )]),
        );

        let bootstrap_json = format!(
            r#"{{"xds_servers":[{{"server_uri":"http://{cp_addr}","channel_creds":[{{"type":"insecure"}}]}}],"node":{{"id":"test"}}}}"#
        );
        let bootstrap = BootstrapConfig::from_json(&bootstrap_json).expect("parse bootstrap");
        let target = XdsUri::parse("xds:///my-service").expect("parse target");
        let channel = XdsChannelBuilder::new(
            XdsChannelConfig::new(target)
                .with_bootstrap(bootstrap)
                .with_max_decoding_message_size(2 * PADDING),
        )
        .build_grpc_channel()
        .expect("build xds channel");

        let mut client = GreeterClient::new(channel);
        let reply = tokio::time::timeout(
            Duration::from_secs(30),
            say_hello_until_prefix(&mut client, "backend:"),
        )
        .await
        .expect("oversized CDS response never routed; decoding limit not applied");
        assert_eq!(reply, "backend: world");

        let _ = backend.shutdown.send(());
    }
}

/// gRFC A65 end-to-end test
#[cfg(feature = "_tls-any")]
mod a65_ads_tls {
    use std::collections::HashMap;
    use std::io::Write;

    use tempfile::NamedTempFile;
    use tokio::time::Duration;
    use tonic::transport::{Certificate, Identity, ServerTlsConfig};
    use xds_test_util::RunningControlPlane;
    use xds_test_util::XdsTestControlPlaneService;
    use xds_test_util::config;

    use crate::BootstrapConfig;
    use crate::XdsChannelBuilder;
    use crate::XdsChannelConfig;
    use crate::XdsUri;

    /// The service the client under test subscribes to.
    const SUBJECT_SERVICE: &str = "my-service";

    struct TestCa {
        params: rcgen::CertificateParams,
        key: rcgen::KeyPair,
        pem: String,
    }

    fn gen_ca() -> TestCa {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["a65-test-ca".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let pem = params.self_signed(&key).unwrap().pem();
        TestCa { params, key, pem }
    }

    /// Issue a leaf cert from `ca`. Returns `(cert_pem, key_pem)`.
    fn gen_leaf(
        ca: &TestCa,
        san: &str,
        purpose: rcgen::ExtendedKeyUsagePurpose,
    ) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![san.to_string()]).unwrap();
        params.extended_key_usages = vec![purpose];
        let issuer = rcgen::Issuer::from_params(&ca.params, &ca.key);
        let cert = params.signed_by(&key, &issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    /// Pin a process-wide rustls crypto provider. With both `tls-ring` and
    /// `tls-aws-lc` compiled in, as under CI's `--all-features`, rustls cannot
    /// choose one on its own and panics.
    fn ensure_crypto_provider() {
        if rustls::crypto::CryptoProvider::get_default().is_some() {
            return;
        }
        #[cfg(feature = "tls-ring")]
        let _ = rustls::crypto::ring::default_provider().install_default();
        #[cfg(all(not(feature = "tls-ring"), feature = "tls-aws-lc"))]
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    /// Start an ADS control plane that serves TLS and requires client certs
    /// signed by `ca`.
    async fn start_mtls_control_plane(ca: &TestCa) -> RunningControlPlane {
        ensure_crypto_provider();
        let (server_cert, server_key) =
            gen_leaf(ca, "127.0.0.1", rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(server_cert, server_key))
            .client_ca_root(Certificate::from_pem(ca.pem.clone()));

        let running = XdsTestControlPlaneService::new()
            .start_with_tls(tls)
            .await
            .expect("start TLS control plane");

        running.get_service().set_xds_config(
            &config::AdsTypeUrl::Lds,
            HashMap::from([(
                SUBJECT_SERVICE.to_string(),
                config::build_inline_listener(SUBJECT_SERVICE, "my-cluster"),
            )]),
        );
        running
    }

    /// Build a channel whose bootstrap requests A65 TLS channel credentials.
    fn build_channel_with_tls_creds(
        control_plane: &RunningControlPlane,
        ca_file: &NamedTempFile,
        cert_file: &NamedTempFile,
        key_file: &NamedTempFile,
    ) -> Result<crate::XdsChannelGrpc, crate::BuildError> {
        let bootstrap_json = serde_json::json!({
            "xds_servers": [{
                "server_uri": control_plane.addr().to_string(),
                "channel_creds": [{"type": "tls", "config": {
                    "ca_certificate_file": ca_file.path().to_str().unwrap(),
                    "certificate_file": cert_file.path().to_str().unwrap(),
                    "private_key_file": key_file.path().to_str().unwrap(),
                }}],
            }],
            "node": {"id": "a65-test"},
        })
        .to_string();

        let bootstrap = BootstrapConfig::from_json(&bootstrap_json).expect("parse bootstrap");
        let target = XdsUri::parse(&format!("xds:///{SUBJECT_SERVICE}")).expect("parse target");
        XdsChannelBuilder::new(XdsChannelConfig::new(target).with_bootstrap(bootstrap))
            .build_grpc_channel()
    }

    /// Poll until `SUBJECT_SERVICE` appears as an LDS subscriber, i.e. the
    /// client's ADS stream completed its TLS handshake. `timeout` is a failure
    /// ceiling, not a wait. Returns `false` on timeout.
    async fn ads_stream_established(
        control_plane: &RunningControlPlane,
        timeout: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if control_plane
                .get_service()
                .has_subscriber_for(&config::AdsTypeUrl::Lds, SUBJECT_SERVICE)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Bootstrap names a CA plus a client identity, and the mTLS-demanding
    /// control plane accepts the stream.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bootstrap_tls_creds_are_used_for_the_ads_connection() {
        let ca = gen_ca();
        let control_plane = start_mtls_control_plane(&ca).await;

        let ca_file = write_temp(&ca.pem);
        let (client_cert, client_key) = gen_leaf(
            &ca,
            "a65-client",
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        );
        let cert_file = write_temp(&client_cert);
        let key_file = write_temp(&client_key);

        let _channel =
            build_channel_with_tls_creds(&control_plane, &ca_file, &cert_file, &key_file)
                .expect("build xds channel");

        assert!(
            ads_stream_established(&control_plane, Duration::from_secs(10)).await,
            "ADS stream never established; the bootstrap CA and client identity \
             were not used for the control plane connection",
        );
    }
}
