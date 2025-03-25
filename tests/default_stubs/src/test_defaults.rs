#![allow(unused_imports)]

use crate::test_client::TestClient;
use crate::*;
use rand::Rng as _;
use std::env;
use std::fs;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tonic::transport::Channel;
use tonic::transport::Server;

#[cfg(test)]
fn echo_requests_iter() -> impl Stream<Item = ()> {
    tokio_stream::iter(1..usize::MAX).map(|_| ())
}

#[cfg(test)]
async fn test_default_stubs(
    mut client: TestClient<Channel>,
    mut client_default_stubs: TestClient<Channel>,
) {
    use tonic::Code;

    // First validate pre-existing functionality (trait has no default implementation, we explicitly return PermissionDenied in lib.rs).
    assert_eq!(
        client.unary(()).await.unwrap_err().code(),
        Code::PermissionDenied
    );
    assert_eq!(
        client.server_stream(()).await.unwrap_err().code(),
        Code::PermissionDenied
    );
    assert_eq!(
        client
            .client_stream(echo_requests_iter().take(5))
            .await
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );
    assert_eq!(
        client
            .bidirectional_stream(echo_requests_iter().take(5))
            .await
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );

    // Then validate opt-in new functionality (trait has default implementation of returning Unimplemented).
    assert_eq!(
        client_default_stubs.unary(()).await.unwrap_err().code(),
        Code::Unimplemented
    );
    assert_eq!(
        client_default_stubs
            .server_stream(())
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    assert_eq!(
        client_default_stubs
            .client_stream(echo_requests_iter().take(5))
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    assert_eq!(
        client_default_stubs
            .bidirectional_stream(echo_requests_iter().take(5))
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
}

#[tokio::test()]
async fn test_default_stubs_tcp() {
    let addrs = run_services_in_background().await;
    let client = test_client::TestClient::connect(format!("http://{}", addrs.0))
        .await
        .unwrap();
    let client_default_stubs = test_client::TestClient::connect(format!("http://{}", addrs.1))
        .await
        .unwrap();
    test_default_stubs(client, client_default_stubs).await;
}

#[tokio::test()]
#[cfg(not(target_os = "windows"))]
async fn test_default_stubs_uds() {
    let addrs = run_services_in_background_uds().await;
    let client = test_client::TestClient::connect(addrs.0).await.unwrap();
    let client_default_stubs = test_client::TestClient::connect(addrs.1).await.unwrap();
    test_default_stubs(client, client_default_stubs).await;
}

#[cfg(test)]
async fn run_services_in_background() -> (SocketAddr, SocketAddr) {
    let svc = test_server::TestServer::new(Svc {});
    let svc_default_stubs = test_default_server::TestDefaultServer::new(Svc {});

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let listener_default_stubs = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_default_stubs = listener_default_stubs.local_addr().unwrap();

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc_default_stubs)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(
                listener_default_stubs,
            ))
            .await
            .unwrap();
    });

    (addr, addr_default_stubs)
}

#[cfg(all(test, not(target_os = "windows")))]
async fn run_services_in_background_uds() -> (String, String) {
    use tokio::net::UnixListener;

    let svc = test_server::TestServer::new(Svc {});
    let svc_default_stubs = test_default_server::TestDefaultServer::new(Svc {});

    let mut rng = rand::thread_rng();
    let suffix: String = (0..8)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect();
    let tmpdir = fs::canonicalize(env::temp_dir())
        .unwrap()
        .join(format!("tonic_test_{}", suffix));
    fs::create_dir(&tmpdir).unwrap();

    let uds_filepath = tmpdir.join("impl.sock").to_str().unwrap().to_string();
    let listener = UnixListener::bind(uds_filepath.as_str()).unwrap();
    let uds_addr = format!("unix://{}", uds_filepath);

    let uds_default_stubs_filepath = tmpdir.join("stub.sock").to_str().unwrap().to_string();
    let listener_default_stubs = UnixListener::bind(uds_default_stubs_filepath.as_str()).unwrap();
    let uds_default_stubs_addr = format!("unix://{}", uds_default_stubs_filepath);

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
            .await
            .unwrap();
    });

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc_default_stubs)
            .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(
                listener_default_stubs,
            ))
            .await
            .unwrap();
    });

    (uds_addr, uds_default_stubs_addr)
}
