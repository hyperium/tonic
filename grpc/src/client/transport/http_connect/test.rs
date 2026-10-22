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

use http::HeaderValue;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

use crate::client::transport::ProxyOptions;
use crate::client::transport::http_connect::HttpConnectHandshaker;
use crate::credentials::ChannelCredentials;
use crate::credentials::LocalChannelCredentials;
use crate::credentials::client::ClientHandshakeInfo;
use crate::credentials::client::HandshakeOutput;
use crate::credentials::common::Authority;
use crate::private;
use crate::rt::EndpointIoStream;
use crate::rt::GrpcRuntime;
use crate::rt::StreamEndpoint;
use crate::rt::tokio::TokioRuntime;

/// Runs a mock target server which expects to read "hello" and replies with
/// "world".
async fn run_mock_server(listener: TcpListener) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buf = vec![0u8; 5];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, b"hello");
    stream.write_all(b"world").await.unwrap();
}

async fn perform_handshake(
    handshaker: &HttpConnectHandshaker,
    proxy_addr: SocketAddr,
    target_port: u16,
) -> Result<HandshakeOutput, String> {
    let source = TcpStream::connect(proxy_addr).await.unwrap();
    let endpoint = StreamEndpoint::new_from_tcp(source).unwrap();

    let info = ClientHandshakeInfo::default();
    let runtime = GrpcRuntime::new(TokioRuntime::default());
    let authority = Authority::new("localhost".to_string(), Some(target_port));

    handshaker
        .connect(
            &authority,
            Box::new(endpoint),
            &info,
            &runtime,
            private::Internal,
        )
        .await
}

async fn verify_client_handshake(
    handshaker: &HttpConnectHandshaker,
    proxy_addr: SocketAddr,
    server_port: u16,
) {
    let handshake_output = perform_handshake(handshaker, proxy_addr, server_port)
        .await
        .unwrap();

    let mut client_stream = EndpointIoStream::new(handshake_output.endpoint);
    client_stream.write_all(b"hello").await.unwrap();
    let mut buf = vec![0u8; 5];
    client_stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, b"world");
}

async fn run_client_handshake_fail(
    handshaker: &HttpConnectHandshaker,
    proxy_addr: SocketAddr,
) -> String {
    match perform_handshake(handshaker, proxy_addr, 12345).await {
        Ok(_) => panic!("Expected connection failure"),
        Err(e) => e,
    }
}

#[tokio::test]
async fn test_proxy_success_no_auth() {
    let local_creds = LocalChannelCredentials::new_arc();

    let server_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = server_listener.local_addr().unwrap();

    let server_handle = tokio::spawn(run_mock_server(server_listener));

    // Start Proxy
    let target_host = format!("localhost:{}", server_addr.port());
    let proxy_addr = spawn_proxy(ProxyConfig {
        expected_host: target_host.clone(),
        expected_auth: None,
        connect_response: connect_success_response(),
        target_addr: Some(server_addr),
    })
    .await;

    // Connect Client via Proxy using HttpConnectHandshaker directly.
    let proxy_options = ProxyOptions::new(target_host, None);
    let handshaker = HttpConnectHandshaker::new(local_creds, &proxy_options);

    verify_client_handshake(&handshaker, proxy_addr, server_addr.port()).await;
    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_proxy_success_with_auth() {
    let local_creds = LocalChannelCredentials::new_arc();

    let server_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = server_listener.local_addr().unwrap();

    let server_handle = tokio::spawn(run_mock_server(server_listener));

    // Start Proxy (expects Basic auth).
    let target_host = format!("localhost:{}", server_addr.port());
    let expected_auth = "Basic dXNlcjpwYXNzd29yZA=="; // user:password
    let proxy_addr = spawn_proxy(ProxyConfig {
        expected_host: target_host.clone(),
        expected_auth: Some(expected_auth.to_string()),
        connect_response: connect_success_response(),
        target_addr: Some(server_addr),
    })
    .await;

    // Connect Client via Proxy with Auth Header.
    let auth_header = HeaderValue::from_str(expected_auth).unwrap();
    let proxy_options = ProxyOptions::new(target_host, Some(auth_header));
    let handshaker = HttpConnectHandshaker::new(local_creds, &proxy_options);

    verify_client_handshake(&handshaker, proxy_addr, server_addr.port()).await;
    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_proxy_failure_large_header() {
    let local_creds = LocalChannelCredentials::new_arc();

    let target_host = "localhost:12345".to_string();
    let proxy_addr = spawn_proxy(ProxyConfig {
        expected_host: target_host.clone(),
        expected_auth: None,
        connect_response: connect_large_header_response(),
        target_addr: None, // Will close after response
    })
    .await;

    let proxy_options = ProxyOptions::new(target_host, None);
    let handshaker = HttpConnectHandshaker::new(local_creds, &proxy_options);

    let err_msg = run_client_handshake_fail(&handshaker, proxy_addr).await;
    assert!(
        err_msg.contains("Response too large"),
        "Expected 'Response too large', got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_proxy_failure_invalid_response() {
    let local_creds = LocalChannelCredentials::new_arc();

    let target_host = "localhost:12345".to_string();
    let proxy_addr = spawn_proxy(ProxyConfig {
        expected_host: target_host.clone(),
        expected_auth: None,
        connect_response: connect_invalid_response(),
        target_addr: None,
    })
    .await;

    let proxy_options = ProxyOptions::new(target_host, None);
    let handshaker = HttpConnectHandshaker::new(local_creds, &proxy_options);

    let err_msg = run_client_handshake_fail(&handshaker, proxy_addr).await;
    assert!(
        err_msg.contains("Failed to parse HTTP response"),
        "Expected parse error, got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_proxy_failure_bad_status() {
    let local_creds = LocalChannelCredentials::new_arc();

    let target_host = "localhost:12345".to_string();
    let proxy_addr = spawn_proxy(ProxyConfig {
        expected_host: target_host.clone(),
        expected_auth: None,
        connect_response: connect_status_response(502, "Bad Gateway"),
        target_addr: None,
    })
    .await;

    let proxy_options = ProxyOptions::new(target_host, None);
    let handshaker = HttpConnectHandshaker::new(local_creds, &proxy_options);

    let err_msg = run_client_handshake_fail(&handshaker, proxy_addr).await;
    assert!(
        err_msg.contains("Proxy returned status 502"),
        "Expected 'Proxy returned status 502', got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_proxy_success_local_rewind_batched() {
    let local_creds = LocalChannelCredentials::new_arc();

    // Start a mock target TCP server that sends the "later" part of the
    // response.
    let target_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target_listener.local_addr().unwrap();

    let initial_bytes = b"initial_batched_bytes_";
    let later_bytes = b"later_server_bytes";

    let target_handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = target_listener.accept().await {
            stream.write_all(later_bytes).await.unwrap();
            stream.flush().await.unwrap();
        }
    });

    let target_host = format!("localhost:{}", target_addr.port());
    let proxy_options = ProxyOptions::new(target_host.clone(), None);
    let handshaker = HttpConnectHandshaker::new(local_creds, &proxy_options);

    // Start Proxy configured to batch initial_bytes and tunnel to target
    // server.
    let proxy_addr = spawn_proxy(ProxyConfig {
        expected_host: target_host.clone(),
        expected_auth: None,
        connect_response: connect_batched_response(initial_bytes),
        target_addr: Some(target_addr),
    })
    .await;

    // Connect Client via Proxy.
    let handshake_output = perform_handshake(&handshaker, proxy_addr, target_addr.port())
        .await
        .unwrap();

    let mut proxied_stream = EndpointIoStream::new(handshake_output.endpoint);

    // Verify we can read BOTH the batched initial_bytes and the subsequent
    // later_bytes.
    let mut expected_total = initial_bytes.to_vec();
    expected_total.extend_from_slice(later_bytes);

    let mut buf = vec![0u8; expected_total.len()];
    proxied_stream.read_exact(&mut buf).await.unwrap();

    assert_eq!(buf, expected_total);
    target_handle.await.unwrap();
}

// --- Helper Functions for Payloads ---

fn connect_success_response() -> Vec<u8> {
    b"HTTP/1.1 200 Connection Established\r\n\r\n".to_vec()
}

fn connect_status_response(code: u16, phrase: &str) -> Vec<u8> {
    format!("HTTP/1.1 {} {}\r\n\r\n", code, phrase).into_bytes()
}

fn connect_large_header_response() -> Vec<u8> {
    let mut res = b"HTTP/1.1 200 OK\r\n".to_vec();
    // Exceed the 8KB buffer limit of HttpConnectHandshaker
    let large_header = vec![b'A'; 9000];
    res.extend_from_slice(b"X-Large: ");
    res.extend_from_slice(&large_header);
    res.extend_from_slice(b"\r\n\r\n");
    res
}

fn connect_invalid_response() -> Vec<u8> {
    b"NOT HTTP RESPONSE\r\n\r\n".to_vec()
}

fn connect_batched_response(server_bytes: &[u8]) -> Vec<u8> {
    let mut res = connect_success_response();
    res.extend_from_slice(server_bytes);
    res
}

// --- Mock Proxy Server ---

struct ProxyConfig {
    expected_host: String,
    expected_auth: Option<String>,
    connect_response: Vec<u8>,
    target_addr: Option<SocketAddr>,
}

async fn spawn_proxy(config: ProxyConfig) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut client_stream, _)) = listener.accept().await {
            let mut buf = vec![0u8; 16384];
            let mut read = 0;

            // Read request
            loop {
                let n = client_stream.read(&mut buf[read..]).await.unwrap();
                if n == 0 {
                    return; // Client disconnected
                }
                read += n;

                let mut headers = [httparse::EMPTY_HEADER; 16];
                let mut req = httparse::Request::new(&mut headers);
                match req.parse(&buf[..read]) {
                    Ok(httparse::Status::Complete(len)) => {
                        // Validate method is CONNECT
                        if req.method != Some("CONNECT") {
                            let res = b"HTTP/1.1 405 Method Not Allowed\r\n\r\n";
                            client_stream.write_all(res).await.unwrap();
                            return;
                        }

                        // Validate path (host).
                        let path = req.path.unwrap();
                        if path != config.expected_host {
                            let res = b"HTTP/1.1 400 Bad Request\r\n\r\n";
                            client_stream.write_all(res).await.unwrap();
                            return;
                        }

                        // Validate Host header.
                        let mut host_ok = false;
                        for header in req.headers.iter() {
                            if header.name.eq_ignore_ascii_case("host")
                                && header.value == config.expected_host.as_bytes()
                            {
                                host_ok = true;
                            }
                        }
                        if !host_ok {
                            let res = b"HTTP/1.1 400 Bad Request\r\n\r\n";
                            client_stream.write_all(res).await.unwrap();
                            return;
                        }

                        // Validate Auth if expected.
                        if let Some(ref expected_auth) = config.expected_auth {
                            let mut auth_ok = false;
                            for header in req.headers.iter() {
                                if header.name.eq_ignore_ascii_case("proxy-authorization")
                                    && header.value == expected_auth.as_bytes()
                                {
                                    auth_ok = true;
                                }
                            }
                            if !auth_ok {
                                let res = b"HTTP/1.1 407 Proxy Authentication Required\nProxy-Authenticate: Basic realm=\"proxy\"\r\n\r\n";
                                client_stream.write_all(res).await.unwrap();
                                return;
                            }
                        }

                        // Send the configured response
                        client_stream
                            .write_all(&config.connect_response)
                            .await
                            .unwrap();
                        client_stream.flush().await.unwrap();

                        // Tunnel if target_addr is Some.
                        if let Some(target_addr) = config.target_addr {
                            let mut backend_stream = match TcpStream::connect(target_addr).await {
                                Ok(s) => s,
                                Err(_) => {
                                    return;
                                }
                            };
                            let _ = tokio::io::copy_bidirectional(
                                &mut client_stream,
                                &mut backend_stream,
                            )
                            .await;
                        }
                        return;
                    }
                    Ok(httparse::Status::Partial) => {
                        if read >= buf.len() {
                            return; // Too large
                        }
                    }
                    Err(_) => {
                        let res = b"HTTP/1.1 400 Bad Request\r\n\r\n";
                        client_stream.write_all(res).await.unwrap();
                        return;
                    }
                }
            }
        }
    });
    addr
}
