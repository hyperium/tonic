use integration_tests::{
    pb::{test_client, test_server, Input, Output},
    BoxFuture,
};
use std::{
    task::{Context, Poll},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};
use tonic::{
    body::Body,
    server::NamedService,
    transport::{server::TcpIncoming, Endpoint, Server},
    Request, Response, Status,
};
use tower_service::Service;

#[derive(Clone)]
struct ExtensionValue(i32);

#[tokio::test]
async fn setting_extension_from_interceptor() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            let value = req.extensions().get::<ExtensionValue>().unwrap();
            assert_eq!(value.0, 42);

            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::with_interceptor(Svc, |mut req: Request<()>| {
        req.extensions_mut().insert(ExtensionValue(42));
        Ok(req)
    });

    let (tx, rx) = oneshot::channel::<()>();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from_listener(listener, true, None).unwrap();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    client.unary_call(Input {}).await.unwrap();

    tx.send(()).unwrap();

    jh.await.unwrap();
}

#[tokio::test]
async fn setting_extension_from_tower() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            let value = req.extensions().get::<ExtensionValue>().unwrap();
            assert_eq!(value.0, 42);

            Ok(Response::new(Output {}))
        }
    }

    let svc = InterceptedService {
        inner: test_server::TestServer::new(Svc),
    };

    let (tx, rx) = oneshot::channel::<()>();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from_listener(listener, true, None).unwrap();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    client.unary_call(Input {}).await.unwrap();

    tx.send(()).unwrap();

    jh.await.unwrap();
}

#[derive(Debug, Clone)]
struct InterceptedService<S> {
    inner: S,
}

impl<S> Service<http::Request<Body>> for InterceptedService<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>>
        + NamedService
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<Body>) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        req.extensions_mut().insert(ExtensionValue(42));

        Box::pin(async move {
            let response = inner.call(req).await?;
            Ok(response)
        })
    }
}

impl<S: NamedService> NamedService for InterceptedService<S> {
    const NAME: &'static str = S::NAME;
}
