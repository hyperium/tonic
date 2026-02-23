/*
 *
 * Copyright 2026 gRPC authors.
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

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;

use rustls::crypto::ring;
use rustls::HandshakeKind;
use rustls::ServerConfig;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::PrivateKeyDer;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use super::*;
use crate::credentials::client::ChannelCredsInternal;
use crate::credentials::client::ClientConnectionSecurityContext;
use crate::credentials::client::ClientHandshakeInfo;
use crate::credentials::common::Authority;
use crate::credentials::tls::RootCertificates;
use crate::credentials::tls::StaticProvider;
use crate::rt;
use crate::rt::TcpOptions;

static INIT: Once = Once::new();

fn init_provider() {
    INIT.call_once(|| {
        let _ = ring::default_provider().install_default();
    });
}

#[tokio::test]
async fn test_tls_handshake() {
    init_provider();
    run_handshake_test(vec![ALPN_PROTO_STR_H2.to_vec()], true).await;
}

#[tokio::test]
async fn test_tls_handshake_no_alpn() {
    init_provider();
    // Server provides NO ALPN. Client requires "h2".
    run_handshake_test(vec![], false).await;
}

#[tokio::test]
async fn test_tls_handshake_bad_alpn() {
    init_provider();
    // Server provides HTTP/1.1 ALPN. Client requires "h2".
    run_handshake_test(vec![b"http/1.1".to_vec()], false).await;
}

#[tokio::test]
async fn test_tls_cipher_suites_secure() {
    init_provider();
    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);
    let config = ClientTlsConfig::new().with_root_certificates_provider(root_provider);

    let provider = rustls::crypto::CryptoProvider::get_default()
        .expect("No default crypto provider found")
        .as_ref()
        .clone();

    // This should succeed as default provider usually has secure suites.
    let creds = RustlsClientTlsCredendials::new_impl(config, provider);
    assert!(
        creds.is_ok(),
        "Failed to create creds with secure provider: {:?}",
        creds.err()
    );
}

#[tokio::test]
async fn test_tls_cipher_suites_insecure() {
    init_provider();
    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);
    let config = ClientTlsConfig::new().with_root_certificates_provider(root_provider);

    let mut provider = rustls::crypto::CryptoProvider::get_default()
        .expect("No default crypto provider found")
        .as_ref()
        .clone();

    // Remove all cipher suites that are considered secure by our policy
    provider.cipher_suites.retain(|suite| !match suite {
        rustls::SupportedCipherSuite::Tls13(_) => true,
        rustls::SupportedCipherSuite::Tls12(suite) => matches!(
            suite.common.suite,
            rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
                | rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
                | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
                | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
                | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256
                | rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
        ),
    });

    let creds = RustlsClientTlsCredendials::new_impl(config, provider);
    assert!(creds.is_err());
    assert_eq!(
        creds.err().unwrap(),
        "Crypto provider has no cipher suites matching the security policy (TLS1.3 or TLS1.2+ECDHE)"
    );
}

#[tokio::test]
async fn test_tls_key_log() {
    init_provider();

    let key_log_dir = std::env::temp_dir();
    let key_log_path = key_log_dir.join("grpc_rust_key_log.txt");

    // Ensure file doesn't exist
    if key_log_path.exists() {
        std::fs::remove_file(&key_log_path).unwrap();
    }

    // Server setup
    let server_config = default_server_config();
    let (addr, task) = setup_server(server_config).await;

    // Client setup
    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);
    let config = ClientTlsConfig::new()
        .with_root_certificates_provider(root_provider)
        .with_key_log_path(key_log_path.clone());

    let creds = RustlsClientTlsCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let endpoint = runtime
        .tcp_stream(addr, TcpOptions::default())
        .await
        .unwrap();
    let authority = Authority::new("localhost".to_string(), Some(addr.port()));

    let result = creds
        .connect(
            &authority,
            endpoint,
            ClientHandshakeInfo::default(),
            runtime,
        )
        .await
        .expect("Handshake failed");
    let mut stream = result.endpoint;
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    assert_eq!(buf, b"Hello world");

    let _ = task.await;

    // Verify key log file exists and has content
    assert!(key_log_path.exists(), "Key log file was not created");
    let content = std::fs::read_to_string(&key_log_path).unwrap();
    assert!(!content.is_empty(), "Key log file is empty");
    // CLIENT_HANDSHAKE_TRAFFIC_SECRET is standard for TLS 1.3
    assert!(
        content.contains("CLIENT_HANDSHAKE_TRAFFIC_SECRET"),
        "Key log missing expected content: {}",
        content
    );

    // Cleanup
    let _ = std::fs::remove_file(key_log_path);
}

#[tokio::test]
async fn test_tls_handshake_wrong_server_name() {
    init_provider();

    // Server setup
    let server_config = default_server_config();
    let (addr, server_task) = setup_server(server_config).await;

    // Client setup
    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);
    let config = ClientTlsConfig::new().with_root_certificates_provider(root_provider);

    let creds = RustlsClientTlsCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let endpoint = runtime
        .tcp_stream(addr, TcpOptions::default())
        .await
        .unwrap();

    let authority = Authority::new(
        // Use a hostname not in the server cert's SANs
        "wrong.host.com".to_string(),
        Some(addr.port()),
    );

    let result = creds
        .connect(
            &authority,
            endpoint,
            ClientHandshakeInfo::default(),
            runtime,
        )
        .await;

    assert!(
        result.is_err(),
        "Handshake should fail with wrong server name"
    );
    let _ = server_task.await;
}

#[tokio::test]
async fn test_tls_validate_authority() {
    init_provider();

    // Server setup
    let server_config = default_server_config();

    let acceptor = TlsAcceptor::from(Arc::new(server_config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            // Complete handshake and hold connection
            if let Ok(mut stream) = acceptor.accept(stream).await {
                // Keep connection open for client to verify
                let _ = stream.read_u8().await;
            }
        }
    });

    // Client setup.
    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);
    let config = ClientTlsConfig::new().with_root_certificates_provider(root_provider);

    let creds = RustlsClientTlsCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let endpoint = runtime
        .tcp_stream(addr, TcpOptions::default())
        .await
        .unwrap();

    let authority = Authority::new("localhost".to_string(), Some(addr.port()));

    let result = creds
        .connect(
            &authority,
            endpoint,
            ClientHandshakeInfo::default(),
            runtime,
        )
        .await
        .expect("Handshake failed");

    let context = result.security.security_context();

    // Validate correct authorities
    assert!(context.validate_authority(&Authority::new("localhost".to_string(), None)));
    assert!(context.validate_authority(&Authority::new("example.com".to_string(), None)));
    assert!(context.validate_authority(&Authority::new("127.0.0.1".to_string(), None)));

    // Validate incorrect authorities
    assert!(!context.validate_authority(&Authority::new("wrong.host".to_string(), None)));
    assert!(!context.validate_authority(&Authority::new("grpc.io".to_string(), None)));
}

#[tokio::test]
async fn test_mtls_handshake_no_identity() {
    init_provider();

    // Server setup (Requires Client Auth)
    let server_config = mtls_server_config();
    let (addr, server_task) = setup_server(server_config).await;

    let config = ClientTlsConfig::new()
        .with_root_certificates_provider(StaticProvider::new(load_root_certs("ca.pem")));

    let creds = RustlsClientTlsCredendials::new(config).unwrap();
    let runtime = rt::default_runtime();
    let endpoint = runtime
        .tcp_stream(addr, TcpOptions::default())
        .await
        .unwrap();
    let authority = Authority::new("localhost".to_string(), Some(addr.port()));

    // In TLS 1.3, the client considers the handshake complete immediately after
    // sending its Certificate and Finished messages. It does not wait for the
    // server to validate them.
    // Consequently, connect() returns success, but the server will subsequently
    // process the credentials, reject them, and close the connection with an
    // Alert. Therefore, the connection succeeds, but the first read on the
    // stream must fail.
    let result = creds
        .connect(
            &authority,
            endpoint,
            ClientHandshakeInfo::default(),
            runtime,
        )
        .await
        .expect("Client handshake expected to succeed with TLS 1.3");

    let mut stream = result.endpoint;
    let mut buf = Vec::new();
    let res = stream.read_to_end(&mut buf).await;
    assert!(
        res.is_err(),
        "read from TLS stream should fail due to missing client identity"
    );

    let _ = server_task.await;
}

#[tokio::test]
async fn test_mtls_handshake_with_identitiy() {
    init_provider();

    // Server setup (Requires Client Auth)
    let server_config = mtls_server_config();
    let (addr, server_task) = setup_server(server_config).await;

    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);

    let identity = load_identity("client1.pem", "client1.key");
    let identity_provider = StaticProvider::new(identity);

    let config = ClientTlsConfig::new()
        .with_root_certificates_provider(root_provider)
        .with_identity_provider(identity_provider);

    let creds = RustlsClientTlsCredendials::new(config).unwrap();
    let runtime = rt::default_runtime();
    let endpoint = runtime
        .tcp_stream(addr, TcpOptions::default())
        .await
        .unwrap();
    let authority = Authority::new("localhost".to_string(), Some(addr.port()));

    let result = creds
        .connect(
            &authority,
            endpoint,
            ClientHandshakeInfo::default(),
            runtime,
        )
        .await
        .expect("Handshake failed with client identity");

    let mut stream = result.endpoint;
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    assert_eq!(buf, b"Hello world");

    let _ = server_task.await;
}

async fn check_client_resumption_disabled(
    versions: Vec<&'static rustls::SupportedProtocolVersion>,
) {
    init_provider();

    // Server setup: Support resumption
    let certs = load_certs("server.pem");
    let key = load_private_key("server.key");
    let provider = ring::default_provider();
    let mut server_config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&versions)
        .expect("invalid versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    server_config.alpn_protocols = vec![ALPN_PROTO_STR_H2.to_vec()];
    // Enable stateful resumption
    server_config.session_storage = rustls::server::ServerSessionMemoryCache::new(32);
    // Enable stateless resumption (TLS 1.3 tickets)
    server_config.send_tls13_tickets = 1;

    let (addr, server_task) = setup_server_multi_connection(server_config, 2).await;

    // Client setup
    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);
    let config = ClientTlsConfig::new().with_root_certificates_provider(root_provider);

    let creds = RustlsClientTlsCredendials::new(config).unwrap();

    for i in 0..2 {
        let runtime = rt::default_runtime();
        let endpoint = runtime
            .tcp_stream(addr, TcpOptions::default())
            .await
            .unwrap();
        let authority = Authority::new("localhost".to_string(), Some(addr.port()));

        let result = creds
            .connect(
                &authority,
                endpoint,
                ClientHandshakeInfo::default(),
                runtime,
            )
            .await
            .expect("Handshake failed");

        let mut tls_stream = result.endpoint;

        let connection = match tls_stream.inner() {
            tokio_rustls::TlsStream::Client(conn) => conn.get_ref().1,
            _ => panic!("Expected client stream"),
        };

        assert_eq!(
            connection.handshake_kind(),
            Some(HandshakeKind::Full),
            "Expected full handshake on attempt {}",
            i
        );

        let mut buf = Vec::new();
        let _ = tls_stream.read_to_end(&mut buf).await;
        assert_eq!(buf, b"Hello world");
    }

    let _ = server_task.await;
}

#[tokio::test]
async fn test_tls_resumption_disabled_tls13() {
    check_client_resumption_disabled(vec![&rustls::version::TLS13]).await;
}

#[tokio::test]
async fn test_tls_resumption_disabled_tls12() {
    check_client_resumption_disabled(vec![&rustls::version::TLS12]).await;
}

fn load_identity(cert_file: &str, key_file: &str) -> Identity {
    let cert = std::fs::read(test_certs_path().join(cert_file)).expect("cannot read cert file");
    let key = std::fs::read(test_certs_path().join(key_file)).expect("cannot read key file");
    Identity::from_pem(cert, key)
}

fn mtls_server_config() -> ServerConfig {
    let certs = load_certs("server.pem");
    let key = load_private_key("server.key");

    let client_ca_path = test_certs_path().join("client_ca.pem");
    let file = std::fs::File::open(client_ca_path).expect("cannot open client CA file");
    let mut reader = std::io::BufReader::new(file);
    let mut root_store = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut reader) {
        root_store.add(cert.unwrap()).unwrap();
    }

    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
        .build()
        .unwrap();

    let mut server_config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .unwrap();
    server_config.alpn_protocols = vec![ALPN_PROTO_STR_H2.to_vec()];
    server_config
}

fn test_certs_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("examples/data/tls")
}

fn load_certs(filename: &str) -> Vec<CertificateDer<'static>> {
    let path = test_certs_path().join(filename);
    let file = std::fs::File::open(path).expect("cannot open certificate file");
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .map(|result| result.unwrap())
        .collect()
}

fn load_private_key(filename: &str) -> PrivateKeyDer<'static> {
    let path = test_certs_path().join(filename);
    let file = std::fs::File::open(path).expect("cannot open private key file");
    let mut reader = std::io::BufReader::new(file);
    loop {
        match rustls_pemfile::read_one(&mut reader).expect("cannot read private key") {
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => return key.into(),
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => return key.into(),
            Some(rustls_pemfile::Item::Sec1Key(key)) => return key.into(),
            None => panic!("no keys found"),
            _ => {}
        }
    }
}

fn load_root_certs(filename: &str) -> RootCertificates {
    let path = test_certs_path().join(filename);
    let ca_pem = std::fs::read(path).unwrap();
    RootCertificates::from_pem(ca_pem)
}

fn default_server_config() -> ServerConfig {
    let certs = load_certs("server.pem");
    let key = load_private_key("server.key");
    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    server_config.alpn_protocols = vec![ALPN_PROTO_STR_H2.to_vec()];
    server_config
}

async fn setup_server(config: ServerConfig) -> (SocketAddr, JoinHandle<()>) {
    setup_server_multi_connection(config, 1).await
}

async fn setup_server_multi_connection(
    config: ServerConfig,
    count: usize,
) -> (SocketAddr, JoinHandle<()>) {
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let task = tokio::spawn(async move {
        for _ in 0..count {
            let (stream, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                match acceptor.accept(stream).await {
                    Ok(mut stream) => {
                        let _ = stream.write_all(b"Hello world").await;
                        let _ = stream.shutdown().await;
                    }
                    Err(err) => {
                        println!("TLS handshake failed: {}", err)
                    }
                }
            });
        }
    });
    (addr, task)
}

async fn run_handshake_test(server_alpn: Vec<Vec<u8>>, expect_success: bool) {
    // Server setup
    let mut server_config = default_server_config();
    server_config.alpn_protocols = server_alpn;
    let (addr, server_task) = setup_server(server_config).await;

    // Client setup
    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);

    let config = ClientTlsConfig::new().with_root_certificates_provider(root_provider);

    let creds = RustlsClientTlsCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let endpoint = runtime
        .tcp_stream(addr, TcpOptions::default())
        .await
        .unwrap();

    let authority = Authority::new("localhost".to_string(), Some(addr.port()));

    let result = creds
        .connect(
            &authority,
            endpoint,
            ClientHandshakeInfo::default(),
            runtime,
        )
        .await;

    if expect_success {
        assert!(result.is_ok(), "Handshake failed: {:?}", result.err());
        let result = result.unwrap();
        let mut stream = result.endpoint;
        let mut buf = Vec::new();
        // Ignore read errors if server closed connection abruptly (which happens in failure cases, but here we expect success)
        let _ = stream.read_to_end(&mut buf).await;
        assert_eq!(buf, b"Hello world");
    } else {
        assert!(result.is_err(), "Handshake succeeded but expected failure");
    }

    let _ = server_task.await;
}
