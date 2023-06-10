use futures_util::FutureExt;
use integration_tests::pb::{test_client, test_server, Input, Output};
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::{
    transport::{server::TcpConnectInfo, Endpoint, Server},
    Request, Response, Status,
};

#[tokio::test]
async fn getting_connect_info() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            assert!(req.local_addr().is_some());
            assert!(req.remote_addr().is_some());
            assert!(req.extensions().get::<TcpConnectInfo>().is_some());

            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1400".parse().unwrap(), rx.map(drop))
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_static("http://127.0.0.1:1400")
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    client.unary_call(Input {}).await.unwrap();

    tx.send(()).unwrap();

    jh.await.unwrap();
}

#[cfg(unix)]
pub mod unix {
    use futures_util::FutureExt;
    use tokio::{
        net::{UnixListener, UnixStream},
        sync::oneshot,
    };
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::{
        transport::{server::UdsConnectInfo, Endpoint, Server, Uri},
        Request, Response, Status,
    };
    use tower::service_fn;

    use integration_tests::pb::{test_client, test_server, Input, Output};

    struct Svc {}

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            let conn_info = req.extensions().get::<UdsConnectInfo>().unwrap();

            // Client-side unix sockets are unnamed.
            assert!(req.local_addr().is_none());
            assert!(req.remote_addr().is_none());
            assert!(conn_info.peer_addr.as_ref().unwrap().is_unnamed());
            // This should contain process credentials for the client socket.
            assert!(conn_info.peer_cred.as_ref().is_some());

            Ok(Response::new(Output {}))
        }
    }

    #[tokio::test]
    async fn getting_connect_info() {
        let mut unix_socket_path = std::env::temp_dir();
        unix_socket_path.push("uds-integration-test");

        let uds = UnixListener::bind(&unix_socket_path).unwrap();
        let uds_stream = UnixListenerStream::new(uds);

        let service = test_server::TestServer::new(Svc {});
        let (tx, rx) = oneshot::channel::<()>();

        let jh = tokio::spawn(async move {
            Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(uds_stream, rx.map(drop))
                .await
                .unwrap();
        });

        // Take a copy before moving into the `service_fn` closure so that the closure
        // can implement `FnMut`.
        let path = unix_socket_path.clone();
        let channel = Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(service_fn(move |_: Uri| UnixStream::connect(path.clone())))
            .await
            .unwrap();

        let mut client = test_client::TestClient::new(channel);

        client.unary_call(Input {}).await.unwrap();

        tx.send(()).unwrap();
        jh.await.unwrap();

        std::fs::remove_file(unix_socket_path).unwrap();
    }
}

#[cfg(unix)]
pub mod vsock {
    use futures_util::FutureExt;
    use tokio::sync::oneshot;
    use tokio_vsock::{VsockListener, VsockStream};
    use tonic::{
        transport::{server::VsockConnectInfo, Endpoint, Server, Uri},
        Request, Response, Status,
    };
    use tower::service_fn;

    use integration_tests::pb::{test_client, test_server, Input, Output};

    // Use vsock-loopback so we don't need to spin up a VM.
    static TEST_CID: u32 = vsock::VMADDR_CID_LOCAL;
    // Arbitrarily chosen.
    static TEST_PORT: u32 = 8000;

    // Virtio VSOCK does not use URIs, hence this URI will never be used.
    // It is defined purely since in order to create a channel, since a URI has to
    // be supplied to create an `Endpoint`.
    static IGNORED_ENDPOINT_URI: &str = "file://[::]:0";

    struct Svc {}

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            let conn_info = req.extensions().get::<VsockConnectInfo>().unwrap();

            assert!(conn_info.local_addr.is_some());
            assert_eq!(conn_info.local_addr.unwrap().cid(), TEST_CID);
            assert_eq!(conn_info.local_addr.unwrap().port(), TEST_PORT);
            assert!(conn_info.peer_addr.is_some());
            assert_eq!(conn_info.peer_addr.unwrap().cid(), TEST_CID);

            Ok(Response::new(Output {}))
        }
    }

    #[tokio::test]
    async fn getting_connect_info() {
        let stream = VsockListener::bind(TEST_CID, TEST_PORT)
            .expect("failed to bind VsockListener")
            .incoming();

        let service = test_server::TestServer::new(Svc {});
        let (tx, rx) = oneshot::channel::<()>();

        let jh = tokio::spawn(async move {
            Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(stream, rx.map(drop))
                .await
                .unwrap();
        });

        let channel = Endpoint::try_from(IGNORED_ENDPOINT_URI)
            .unwrap()
            .connect_with_connector(service_fn(move |_: Uri| {
                VsockStream::connect(TEST_CID, TEST_PORT)
            }))
            .await
            .unwrap();

        let mut client = test_client::TestClient::new(channel);

        client.unary_call(Input {}).await.unwrap();

        tx.send(()).unwrap();
        jh.await.unwrap();
    }
}
