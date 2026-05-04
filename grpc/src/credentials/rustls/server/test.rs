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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;

use rustls::HandshakeKind;
use rustls::crypto::ring;
use rustls_pki_types::ServerName;
use tempfile::NamedTempFile;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::credentials::ServerCredentials;
use crate::credentials::rustls::ALPN_PROTO_STR_H2;
use crate::credentials::rustls::Identity;
use crate::credentials::rustls::RootCertificates;
use crate::credentials::rustls::StaticProvider;
use crate::credentials::rustls::server::RustlsServerCredendials;
use crate::credentials::rustls::server::ServerTlsConfig;
use crate::credentials::rustls::server::TlsClientCertificateRequestType;
use crate::private;
use crate::rt::AsyncIoAdapter;
use crate::rt::tokio::TokioIoStream;
use crate::rt::{self};

static INIT: Once = Once::new();

fn init_provider() {
    INIT.call_once(|| {
        let _ = ring::default_provider().install_default();
    });
}

#[tokio::test]
async fn test_tls_server_handshake() {
    init_provider();
    let client_alpn = vec![ALPN_PROTO_STR_H2.to_vec()];

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);
    let config = ServerTlsConfig::new(identity_provider);
    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let result = creds.accept(stream, runtime, private::Internal).await;
        assert!(
            result.is_ok(),
            "Server handshake failed: {:?}",
            result.err()
        );
        let mut stream = AsyncIoAdapter::new(result.unwrap().endpoint);
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping!");
        stream.write_all(b"pong!").await.unwrap();
    });

    // Client setup
    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    client_config.alpn_protocols = client_alpn;

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    let result = connector.connect(domain, stream).await;

    assert!(
        result.is_ok(),
        "Client handshake failed: {:?}",
        result.err()
    );

    let mut tls_stream = result.unwrap();
    tls_stream.write_all(b"ping!").await.unwrap();
    let mut buf = [0u8; 5];
    tls_stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"pong!");

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_handshake_no_alpn() {
    init_provider();

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);
    let config = ServerTlsConfig::new(identity_provider);
    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let result = creds.accept(stream, runtime, private::Internal).await;
        assert!(result.is_err(), "Server handshake should have failed");
    });

    // Client setup
    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    client_config.alpn_protocols = vec![];

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    let result = connector.connect(domain, stream).await;

    // Handshake will fail after the handshake is complete no ALPN is skipped.
    let mut tls_stream = result.unwrap();
    let _ = tls_stream.write_all(b"ping!").await;
    let mut buf = [0u8; 5];
    let res = tls_stream.read_exact(&mut buf).await;
    assert!(res.is_err());

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_handshake_bad_alpn() {
    init_provider();
    let client_alpn = vec![b"http/1.1".to_vec()];

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);
    let config = ServerTlsConfig::new(identity_provider);
    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let runtime = rt::default_runtime();
        let result = creds.accept(stream, runtime, private::Internal).await;
        assert!(result.is_err(), "Server handshake should have failed");
    });

    // Client setup
    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    client_config.alpn_protocols = client_alpn;

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    // Handshake should fail due to incompatible application protocols.
    let result = connector.connect(domain, stream).await;
    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_handshake_alpn_h1_and_h2() {
    init_provider();
    let client_alpn = vec![b"http/1.1".to_vec(), ALPN_PROTO_STR_H2.to_vec()];

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);
    let config = ServerTlsConfig::new(identity_provider);
    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let runtime = rt::default_runtime();
        creds
            .accept(stream, runtime, private::Internal)
            .await
            .unwrap();
    });

    // Client setup
    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    client_config.alpn_protocols = client_alpn;

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    // Handshake should succeed.
    let result = connector.connect(domain, stream).await.unwrap();
    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_mtls_require_fail() {
    init_provider();

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);

    let root_certs = load_root_certs("ca.pem");
    let root_provider = StaticProvider::new(root_certs);

    let config = ServerTlsConfig::new(identity_provider).with_request_type(
        TlsClientCertificateRequestType::RequireAndVerify {
            roots_provider: root_provider,
        },
    );

    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let result = creds.accept(stream, runtime, private::Internal).await;
        assert!(result.is_err(), "Handshake should fail without client cert");
    });

    // Client setup: No identity
    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    client_config.alpn_protocols = vec![b"h2".to_vec()];

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    let result = connector.connect(domain, stream).await;

    // In TLS 1.3 client assumes the handshake succeeded but first read/write
    // fails.
    let mut tls_stream = result.unwrap();
    let mut buf = [0u8; 1];
    let res = tls_stream.read(&mut buf).await;
    assert!(res.is_err());

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_mtls_success() {
    init_provider();

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);

    let root_certs = load_root_certs("client_ca.pem");
    let root_provider = StaticProvider::new(root_certs);

    let config = ServerTlsConfig::new(identity_provider).with_request_type(
        TlsClientCertificateRequestType::RequireAndVerify {
            roots_provider: root_provider,
        },
    );

    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let result = creds
            .accept(stream, runtime, private::Internal)
            .await
            .expect("Server handshake failed");
        let mut stream = AsyncIoAdapter::new(result.endpoint);
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping!");
        stream.write_all(b"pong!").await.unwrap();
    });

    // Client setup: With identity
    let client_identity_cert = load_certs("client1.pem");
    let client_identity_key = load_private_key("client1.key");

    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_client_auth_cert(client_identity_cert, client_identity_key)
        .unwrap();
    let mut client_config = client_config;
    client_config.alpn_protocols = vec![b"h2".to_vec()];

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    let mut tls_stream = connector.connect(domain, stream).await.unwrap();
    tls_stream.write_all(b"ping!").await.unwrap();
    let mut buf = [0u8; 5];
    tls_stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"pong!");

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_mtls_optional() {
    init_provider();

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);

    let root_certs = load_root_certs("client_ca.pem");
    let root_provider = StaticProvider::new(root_certs);

    let config = ServerTlsConfig::new(identity_provider).with_request_type(
        TlsClientCertificateRequestType::RequestAndVerify {
            roots_provider: root_provider,
        },
    );

    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let result = creds
            .accept(stream, runtime, private::Internal)
            .await
            .expect("Server handshake failed");
        let mut stream = AsyncIoAdapter::new(result.endpoint);
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping!");
        stream.write_all(b"pong!").await.unwrap();
    });

    // Client setup: Without identity
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    let mut client_config = client_config;
    client_config.alpn_protocols = vec![b"h2".to_vec()];

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    let mut tls_stream = connector.connect(domain, stream).await.unwrap();
    tls_stream.write_all(b"ping!").await.unwrap();
    let mut buf = [0u8; 5];
    tls_stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"pong!");

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_key_log() {
    init_provider();
    let key_log_file = NamedTempFile::new().expect("failed to create a temporary file.");

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);
    let config =
        ServerTlsConfig::new(identity_provider).insecure_with_key_log_path(key_log_file.path());

    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = TokioIoStream::new_from_tcp(stream).unwrap();
        let result = creds
            .accept(stream, runtime, private::Internal)
            .await
            .expect("Server handshake failed");
        let mut stream = AsyncIoAdapter::new(result.endpoint);
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping!");
        stream.write_all(b"pong!").await.unwrap();
    });

    // Client setup
    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    client_config.alpn_protocols = vec![b"h2".to_vec()];

    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await.unwrap();
    let domain = ServerName::try_from("localhost").unwrap();

    let mut tls_stream = connector.connect(domain, stream).await.unwrap();
    tls_stream.write_all(b"ping!").await.unwrap();
    let mut buf = [0u8; 5];
    tls_stream.read_exact(&mut buf).await.unwrap();

    server_task.await.unwrap();

    // Verify key log file exists and has content
    let content = std::fs::read_to_string(key_log_file.path()).unwrap();
    assert!(!content.is_empty(), "Key log file is empty");
    assert!(
        content.contains("SERVER_HANDSHAKE_TRAFFIC_SECRET"),
        "Key log missing expected content: {}",
        content
    );
}

async fn check_resumption_disabled(versions: Vec<&'static rustls::SupportedProtocolVersion>) {
    init_provider();

    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);
    let config = ServerTlsConfig::new(identity_provider);
    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let stream = TokioIoStream::new_from_tcp(stream).unwrap();
            let runtime = rt::default_runtime();
            let result = creds.accept(stream, runtime, private::Internal).await;
            assert!(result.is_ok());
            let stream = result.unwrap().endpoint;
            AsyncIoAdapter::new(stream)
                .write_all(b"pong!")
                .await
                .unwrap();
        }
    });

    // Client setup with session cache
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap();
    let mut client_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&versions)
        .expect("invalid versions")
        .with_root_certificates(create_root_store())
        .with_no_client_auth();

    client_config.resumption = rustls::client::Resumption::in_memory_sessions(32);

    client_config.alpn_protocols = vec![ALPN_PROTO_STR_H2.to_vec()];
    let connector = TlsConnector::from(Arc::new(client_config));

    for i in 0..2 {
        let stream = TcpStream::connect(addr).await.unwrap();
        let domain = ServerName::try_from("localhost").unwrap();
        let mut tls_stream = connector.connect(domain, stream).await.unwrap();

        let (_, conn) = tls_stream.get_ref();
        assert_eq!(
            conn.handshake_kind(),
            Some(HandshakeKind::Full),
            "Expected full handshake on attempt {}",
            i
        );

        let mut buf = [0u8; 5];
        tls_stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong!");
    }

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_resumption_disabled_tls13() {
    check_resumption_disabled(vec![&rustls::version::TLS13]).await;
}

#[tokio::test]
async fn test_tls_server_resumption_disabled_tls12() {
    check_resumption_disabled(vec![&rustls::version::TLS12]).await;
}

#[tokio::test]
async fn test_tls_server_sni() {
    init_provider();

    let identity1 = load_identity("server.pem", "server.key");
    let identity2 = load_identity("server2.pem", "server2.key");
    // identity2 has *.test.com
    let identity_provider = StaticProvider::new(vec![identity1, identity2]);
    let config = ServerTlsConfig::new(identity_provider);
    let creds = RustlsServerCredendials::new(config).unwrap();

    let runtime = rt::default_runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let stream = TokioIoStream::new_from_tcp(stream).unwrap();
            let runtime = rt::default_runtime();
            let result = creds.accept(stream, runtime, private::Internal).await;
            assert!(
                result.is_ok(),
                "Server handshake failed: {:?}",
                result.err()
            );
            let mut stream = AsyncIoAdapter::new(result.unwrap().endpoint);
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping!");
            stream.write_all(b"pong!").await.unwrap();
        }
    });

    // Client setup
    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(create_root_store())
        .with_no_client_auth();
    client_config.alpn_protocols = vec![ALPN_PROTO_STR_H2.to_vec()];
    let connector = TlsConnector::from(Arc::new(client_config));

    let test_com = ServerName::try_from("abc.test.com").unwrap();

    // Request 1: abc.example.com
    {
        let stream = TcpStream::connect(addr).await.unwrap();
        let domain = ServerName::try_from("abc.example.com").unwrap();
        let mut tls_stream = connector.connect(domain, stream).await.unwrap();

        let (_, conn) = tls_stream.get_ref();
        let certs = conn.peer_certificates().unwrap();
        let end_entity = webpki::EndEntityCert::try_from(&certs[0]).unwrap();

        // verify it doesn't have a DNS name of *.test.com
        assert!(
            end_entity
                .verify_is_valid_for_subject_name(&test_com)
                .is_err()
        );

        tls_stream.write_all(b"ping!").await.unwrap();
        let mut buf = [0u8; 5];
        tls_stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong!");
    }

    // Request 2: abc.test.com
    {
        let stream = TcpStream::connect(addr).await.unwrap();
        let domain = ServerName::try_from("abc.test.com").unwrap();
        let mut tls_stream = connector.connect(domain, stream).await.unwrap();

        let (_, conn) = tls_stream.get_ref();
        let certs = conn.peer_certificates().unwrap();
        let end_entity = webpki::EndEntityCert::try_from(&certs[0]).unwrap();

        // verify that the peer has a certificate with DNS name of *.test.com
        assert!(
            end_entity
                .verify_is_valid_for_subject_name(&test_com)
                .is_ok()
        );

        tls_stream.write_all(b"ping!").await.unwrap();
        let mut buf = [0u8; 5];
        tls_stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong!");
    }

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_tls_server_cipher_suites_insecure() {
    init_provider();
    let identity = load_identity("server.pem", "server.key");
    let identity_provider = StaticProvider::new(vec![identity]);
    let config = ServerTlsConfig::new(identity_provider);

    let mut provider = rustls::crypto::CryptoProvider::get_default()
        .expect("No default crypto provider found")
        .as_ref()
        .clone();

    fn is_secure(suported_suite: &rustls::SupportedCipherSuite) -> bool {
        match suported_suite {
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
        }
    }

    // Remove all cipher suites that are considered secure by gRPC.
    provider.cipher_suites.retain(|suite| !is_secure(suite));

    let creds = RustlsServerCredendials::new_impl(config, provider);
    assert!(creds.err().unwrap().contains("no cipher suites matching"));
}

fn create_root_store() -> rustls::RootCertStore {
    let root_certs = load_certs("ca.pem");
    let mut root_store = rustls::RootCertStore::empty();
    for cert in root_certs {
        root_store.add(cert).unwrap();
    }
    root_store
}

fn test_certs_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("examples/data/tls")
}

fn load_certs(filename: &str) -> Vec<rustls_pki_types::CertificateDer<'static>> {
    let path = test_certs_path().join(filename);
    let file = std::fs::File::open(path).expect("cannot open certificate file");
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .map(|result| result.unwrap())
        .collect()
}

fn load_private_key(filename: &str) -> rustls_pki_types::PrivateKeyDer<'static> {
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

fn load_identity(cert_file: &str, key_file: &str) -> Identity {
    let cert = std::fs::read(test_certs_path().join(cert_file)).expect("cannot read cert file");
    let key = std::fs::read(test_certs_path().join(key_file)).expect("cannot read key file");
    Identity::from_pem(cert, key)
}
