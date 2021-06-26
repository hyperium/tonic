#![allow(unused_imports)]

tonic::include_proto!("test");

use std::sync::{
    atomic::{AtomicUsize, Ordering::Relaxed},
    Arc,
};
use tokio::net::TcpListener;
use tonic::{
    transport::{Channel, Server},
    Request, Response, Status,
};
use tower::{layer::layer_fn, Service, ServiceBuilder};
use tower_http::map_response_body::MapResponseBodyLayer;

mod util;

// TODO(david): client copmressing messages
// TODO(david): client streaming
// TODO(david): server streaming
// TODO(david): bidirectional streaming

struct Svc;

const UNCOMPRESSED_MIN_BODY_SIZE: usize = 1024;

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE];
        Ok(Response::new(Output {
            data: data.to_vec(),
        }))
    }
}

// TODO(david): document that using a multi threaded tokio runtime is
// required (because of `block_in_place`)
#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
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
            assert_eq!(req.headers().get("grpc-accept-encoding").unwrap(), "gzip");
            self.0.call(req)
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(layer_fn(AssertCorrectAcceptEncoding))
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: bytes_sent_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .send_gzip()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        }
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .accept_gzip()
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    let res = client.unary_call(Request::new(Input {})).await.unwrap();

    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");

    let bytes_sent = bytes_sent_counter.load(Relaxed);
    assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled() {
    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                // no compression enable on the server so responses should not be compressed
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: bytes_sent_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        }
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .accept_gzip()
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    let res = client.unary_call(Request::new(Input {})).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = bytes_sent_counter.load(Relaxed);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disabled() {
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

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(layer_fn(AssertCorrectAcceptEncoding))
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: bytes_sent_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .send_gzip()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        }
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    let res = client.unary_call(Request::new(Input {})).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = bytes_sent_counter.load(Relaxed);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}
