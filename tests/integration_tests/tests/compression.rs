use integration_tests::pb::{test_client, test_server, Input, Output};
use tokio::net::TcpListener;
use tonic::{
    transport::{Channel, Server},
    Request, Response, Status,
};
use tower::Service;

// TODO(david): client copmressing messages
// TODO(david): client streaming
// TODO(david): server streaming
// TODO(david): bidirectional streaming

// TODO(david): document that using a multi threaded tokio runtime is
// required (because of `block_in_place`)
#[tokio::test(flavor = "multi_thread")]
async fn server_compressing_messages() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    #[derive(Clone, Copy)]
    struct AssertCorrectAcceptEncoding<S>(S);

    impl<S, B> Service<http::Request<B>> for AssertCorrectAcceptEncoding<S>
    where
        S: Service<http::Request<B>>,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = S::Future;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            println!("test middleware called");
            assert_eq!(req.headers().get("grpc-accept-encoding").unwrap(), "gzip");

            self.0.call(req)
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        Server::builder()
            .layer(tower::layer::layer_fn(AssertCorrectAcceptEncoding))
            .send_gzip()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .accept_gzip()
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    let res = client.unary_call(Request::new(Input {})).await.unwrap();

    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        // no compression enable on the server so responses should not be compressed
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .accept_gzip()
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    let res = client.unary_call(Request::new(Input {})).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disabled() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    #[derive(Clone, Copy)]
    struct AssertCorrectAcceptEncoding<S>(S);

    impl<S, B> Service<http::Request<B>> for AssertCorrectAcceptEncoding<S>
    where
        S: Service<http::Request<B>>,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = S::Future;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            assert!(req.headers().get("grpc-accept-encoding").is_none());

            self.0.call(req)
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        Server::builder()
            .layer(tower::layer::layer_fn(AssertCorrectAcceptEncoding))
            .send_gzip()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    let res = client.unary_call(Request::new(Input {})).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());
}
