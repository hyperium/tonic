use integration_tests::pb::{test_client::TestClient, test_server, Input, Output};
use integration_tests::BoxFuture;
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::{net::TcpListener, sync::oneshot};
use tonic::body::Body;
use tonic::transport::server::TcpConnectInfo;
use tonic::{
    transport::{server::TcpIncoming, Endpoint, Server},
    Code, Request, Response, Status,
};

struct Svc(Arc<Mutex<Option<oneshot::Sender<()>>>>);

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
        let mut l = self.0.lock().unwrap();
        l.take().unwrap().send(()).unwrap();

        Ok(Response::new(Output {}))
    }
}

#[tokio::test]
async fn connect_returns_err() {
    let res = TestClient::connect("http://thisdoesntexist.test").await;

    assert!(res.is_err());
}

#[tokio::test]
async fn connect_handles_tls() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .unwrap();
    TestClient::connect("https://github.com").await.unwrap();
}

#[tokio::test]
async fn connect_returns_err_via_call_after_connected() {
    let (tx, rx) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(tx)));
    let svc = test_server::TestServer::new(Svc(sender));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = TestClient::connect(format!("http://{addr}")).await.unwrap();

    // First call should pass, then shutdown the server
    client.unary_call(Request::new(Input {})).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let res = client.unary_call(Request::new(Input {})).await;

    let err = res.unwrap_err();
    assert_eq!(err.code(), Code::Unavailable);

    jh.await.unwrap();
}

#[tokio::test]
async fn connect_lazy_reconnects_after_first_failure() {
    let (tx, rx) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(tx)));
    let svc = test_server::TestServer::new(Svc(sender));

    {
        let channel = Endpoint::from_static("http://127.0.0.1:0").connect_lazy();
        let mut client = TestClient::new(channel);

        // First call should fail, the server is not running
        client.unary_call(Request::new(Input {})).await.unwrap_err();
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    // Start the server now, second call should succeed
    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_lazy();

    let mut client = TestClient::new(channel);

    tokio::time::sleep(Duration::from_millis(100)).await;
    client.unary_call(Request::new(Input {})).await.unwrap();

    // The server shut down, third call should fail
    tokio::time::sleep(Duration::from_millis(100)).await;
    let err = client.unary_call(Request::new(Input {})).await.unwrap_err();

    assert_eq!(err.code(), Code::Unavailable);

    jh.await.unwrap();
}

#[tokio::test]
async fn connect_lazy_reconnects_after_connection_denied() {
    integration_tests::trace_init();

    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    #[derive(Clone)]
    struct TestLayer;

    impl<S> tower::Layer<S> for TestLayer {
        type Service = TestService<S>;

        fn layer(&self, inner: S) -> Self::Service {
            let sent = Arc::new(Mutex::new(HashSet::new()));
            TestService { inner, sent }
        }
    }

    /// Service that keeps connection alive but rejects all requests until the connection is restarted
    #[derive(Clone)]
    struct TestService<S> {
        inner: S,
        sent: Arc<Mutex<HashSet<u16>>>,
    }

    impl<S> tower::Service<http::Request<Body>> for TestService<S>
    where
        S: tower::Service<http::Request<Body>, Response = http::Response<Body>>,
        S::Error: std::fmt::Debug,
        S::Future: Send + 'static,
    {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<Body>) -> Self::Future {
            let conn = req.extensions().get::<TcpConnectInfo>().unwrap();
            let port = conn.remote_addr.unwrap().port();

            let mut sent = self.sent.lock().unwrap();
            match sent.contains(&port) {
                true => Box::pin(async {
                    Ok(http::Response::builder()
                        .status(http::StatusCode::BAD_GATEWAY)
                        .body(Body::empty())
                        .unwrap())
                }),
                false => {
                    sent.insert(port);
                    let fut = self.inner.call(req);
                    Box::pin(async move { Ok(fut.await.unwrap()) })
                }
            }
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    tokio::spawn(async move {
        Server::builder()
            .layer(TestLayer)
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = TestClient::connect(format!("http://{addr}")).await.unwrap();

    // Expect that we receive an initial response followed by a server error
    client.unary_call(Input {}).await.unwrap();
    client.unary_call(Input {}).await.unwrap_err();

    // Expect reconnect, without reconnect we would see the error condition persist
    client.unary_call(Input {}).await.unwrap();
    client.unary_call(Input {}).await.unwrap_err();
}
