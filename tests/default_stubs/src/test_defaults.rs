#![allow(unused_imports)]

use crate::*;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tonic::transport::Server;

#[cfg(test)]
fn echo_requests_iter() -> impl Stream<Item = ()> {
    tokio_stream::iter(1..usize::MAX).map(|_| ())
}

#[tokio::test()]
async fn test_default_stubs() {
    use tonic::Code;

    let addrs = run_services_in_background().await;

    // First validate pre-existing functionality (trait has no default implementation, we explicitly return PermissionDenied in lib.rs).
    let mut client = test_client::TestClient::connect(format!("http://{}", addrs.0))
        .await
        .unwrap();
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
    let mut client_default_stubs = test_client::TestClient::connect(format!("http://{}", addrs.1))
        .await
        .unwrap();
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
