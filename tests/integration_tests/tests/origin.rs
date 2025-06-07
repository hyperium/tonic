use integration_tests::pb::test_client;
use integration_tests::pb::{test_server, Input, Output};
use integration_tests::BoxFuture;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use tokio::{net::TcpListener, sync::oneshot};
use tonic::codegen::http::Request;
use tonic::{
    transport::{server::TcpIncoming, Endpoint, Server},
    Response, Status,
};
use tower::Layer;
use tower::Service;

#[tokio::test]
async fn writes_origin_header() {
    struct Svc;

    impl test_server::Test for Svc {
        async fn unary_call(
            &self,
            _req: tonic::Request<Input>,
        ) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    let jh = tokio::spawn(async move {
        Server::builder()
            .layer(OriginLayer {})
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .origin("https://docs.rs".parse().expect("valid uri"))
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    match client.unary_call(Input {}).await {
        Ok(_) => {}
        Err(status) => panic!("{}", status.message()),
    }

    tx.send(()).unwrap();

    jh.await.unwrap();
}

#[derive(Clone)]
struct OriginLayer {}

impl<S> Layer<S> for OriginLayer {
    type Service = OriginService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OriginService { inner }
    }
}

#[derive(Clone)]
struct OriginService<S> {
    inner: S,
}

impl<T> Service<Request<tonic::body::Body>> for OriginService<T>
where
    T: Service<Request<tonic::body::Body>>,
    T::Future: Send + 'static,
    T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = T::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<tonic::body::Body>) -> Self::Future {
        assert_eq!(req.uri().host(), Some("docs.rs"));
        let fut = self.inner.call(req);

        Box::pin(async move { fut.await.map_err(Into::into) })
    }
}
