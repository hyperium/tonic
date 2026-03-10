use bytes::Bytes;
use http::Uri;
use hyper_util::rt::TokioIo;
use integration_tests::mock::MockStream;
use integration_tests::pb::{
    test_client, test_server, test_stream_client, test_stream_server, Input, InputStream, Output,
    OutputStream,
};
use integration_tests::BoxFuture;
use std::error::Error;
use std::task::{Context as StdContext, Poll as StdPoll};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::{net::TcpListener, sync::oneshot};
use tonic::body::Body;
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::{
    transport::{server::TcpIncoming, Endpoint, Server},
    Code, Request, Response, Status,
};

#[tokio::test]
async fn status_with_details() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            Err(Status::with_details(
                Code::ResourceExhausted,
                "Too many requests",
                Bytes::from_static(&[1]),
            ))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

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

    let mut channel = test_client::TestClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    let err = channel
        .unary_call(Request::new(Input {}))
        .await
        .unwrap_err();

    assert_eq!(err.message(), "Too many requests");
    assert_eq!(err.details(), &[1]);

    tx.send(()).unwrap();

    jh.await.unwrap();
}

#[tokio::test]
async fn status_with_metadata() {
    const MESSAGE: &str = "Internal error, see metadata for details";

    const ASCII_NAME: &str = "x-host-ip";
    const ASCII_VALUE: &str = "127.0.0.1";

    const BINARY_NAME: &str = "x-host-name-bin";
    const BINARY_VALUE: &[u8] = b"localhost";

    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            let mut metadata = MetadataMap::new();
            metadata.insert(ASCII_NAME, ASCII_VALUE.parse().unwrap());
            metadata.insert_bin(BINARY_NAME, MetadataValue::from_bytes(BINARY_VALUE));

            Err(Status::with_metadata(Code::Internal, MESSAGE, metadata))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

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

    let mut channel = test_client::TestClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    let err = channel
        .unary_call(Request::new(Input {}))
        .await
        .unwrap_err();

    assert_eq!(err.code(), Code::Internal);
    assert_eq!(err.message(), MESSAGE);

    let metadata = err.metadata();

    assert_eq!(
        metadata.get(ASCII_NAME).unwrap().to_str().unwrap(),
        ASCII_VALUE
    );

    assert_eq!(
        metadata.get_bin(BINARY_NAME).unwrap().to_bytes().unwrap(),
        BINARY_VALUE
    );

    tx.send(()).unwrap();

    jh.await.unwrap();
}

type Stream<T> = std::pin::Pin<
    Box<dyn tokio_stream::Stream<Item = std::result::Result<T, Status>> + Send + 'static>,
>;

#[tokio::test]
async fn status_from_server_stream() {
    integration_tests::trace_init();

    struct Svc;

    #[tonic::async_trait]
    impl test_stream_server::TestStream for Svc {
        type StreamCallStream = Stream<OutputStream>;

        async fn stream_call(
            &self,
            _: Request<InputStream>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            let s = tokio_stream::iter(vec![
                Err::<OutputStream, _>(Status::unavailable("foo")),
                Err::<OutputStream, _>(Status::unavailable("bar")),
            ]);
            Ok(Response::new(Box::pin(s) as Self::StreamCallStream))
        }
    }

    let svc = test_stream_server::TestStreamServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = test_stream_client::TestStreamClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    let mut stream = client
        .stream_call(InputStream {})
        .await
        .unwrap()
        .into_inner();

    assert_eq!(stream.message().await.unwrap_err().message(), "foo");
    assert_eq!(stream.message().await.unwrap(), None);
}

#[tokio::test]
async fn status_from_server_stream_with_source() {
    integration_tests::trace_init();

    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector_lazy(tower::service_fn(move |_: Uri| async move {
            Err::<TokioIo<MockStream>, _>(std::io::Error::other("WTF"))
        }));

    let mut client = test_stream_client::TestStreamClient::new(channel);

    let error = client.stream_call(InputStream {}).await.unwrap_err();

    let source = error.source().unwrap();
    source.downcast_ref::<tonic::transport::Error>().unwrap();
}

#[tokio::test]
async fn status_from_server_stream_with_inferred_status() {
    integration_tests::trace_init();

    struct Svc;

    #[tonic::async_trait]
    impl test_stream_server::TestStream for Svc {
        type StreamCallStream = Stream<OutputStream>;

        async fn stream_call(
            &self,
            _: Request<InputStream>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            let s = tokio_stream::once(Ok(OutputStream {}));
            Ok(Response::new(Box::pin(s) as Self::StreamCallStream))
        }
    }

    #[derive(Clone)]
    struct TestLayer;

    impl<S> tower::Layer<S> for TestLayer {
        type Service = TestService;

        fn layer(&self, _: S) -> Self::Service {
            TestService
        }
    }

    #[derive(Clone)]
    struct TestService;

    impl tower::Service<http::Request<Body>> for TestService {
        type Response = http::Response<Body>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: http::Request<Body>) -> Self::Future {
            Box::pin(async {
                Ok(http::Response::builder()
                    .status(http::StatusCode::BAD_GATEWAY)
                    .body(Body::empty())
                    .unwrap())
            })
        }
    }

    let svc = test_stream_server::TestStreamServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming: TcpIncoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    tokio::spawn(async move {
        Server::builder()
            .layer(TestLayer)
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = test_stream_client::TestStreamClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    let mut stream = client
        .stream_call(InputStream {})
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        stream.message().await.unwrap_err().code(),
        Code::Unavailable
    );

    assert_eq!(stream.message().await.unwrap(), None);
}

#[tokio::test]
async fn message_and_then_status_from_server_stream() {
    integration_tests::trace_init();

    struct Svc;

    #[tonic::async_trait]
    impl test_stream_server::TestStream for Svc {
        type StreamCallStream = Stream<OutputStream>;

        async fn stream_call(
            &self,
            _: Request<InputStream>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            let s = tokio_stream::iter(vec![
                Ok(OutputStream {}),
                Err::<OutputStream, _>(Status::unavailable("foo")),
            ]);
            Ok(Response::new(Box::pin(s) as Self::StreamCallStream))
        }
    }

    let svc = test_stream_server::TestStreamServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = test_stream_client::TestStreamClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    let mut stream = client
        .stream_call(InputStream {})
        .await
        .unwrap()
        .into_inner();

    assert_eq!(stream.message().await.unwrap(), Some(OutputStream {}));
    assert_eq!(stream.message().await.unwrap_err().message(), "foo");
    assert_eq!(stream.message().await.unwrap(), None);
}

// ---------------------------------------------------------------------------
// Bug fix: HTTP 200 response with no grpc-status trailer must surface as
// Internal, not silently treated as a clean end-of-stream.
//
// We simulate this by interposing a tower layer that replaces the real
// server response body with one that immediately ends (no frames, no
// trailers) while keeping the HTTP 200 status code.
// ---------------------------------------------------------------------------

/// A body that yields no frames and ends immediately, simulating a stream
/// that was truncated before any trailers were sent.
struct TruncatedBody;

impl http_body::Body for TruncatedBody {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut StdContext<'_>,
    ) -> StdPoll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        // Immediately signal end-of-stream with no trailers.
        StdPoll::Ready(None)
    }
}

#[tokio::test]
async fn missing_grpc_status_trailer_is_internal_error() {
    integration_tests::trace_init();

    struct Svc;

    #[tonic::async_trait]
    impl test_stream_server::TestStream for Svc {
        type StreamCallStream = std::pin::Pin<
            Box<
                dyn tokio_stream::Stream<Item = Result<OutputStream, tonic::Status>>
                    + Send
                    + 'static,
            >,
        >;

        async fn stream_call(
            &self,
            _: tonic::Request<InputStream>,
        ) -> Result<tonic::Response<Self::StreamCallStream>, tonic::Status> {
            // This stream would normally send one message and then a proper
            // grpc-status trailer.  The intercept layer below replaces the
            // response body before it ever reaches the client.
            let s = tokio_stream::iter(vec![Ok(OutputStream {})]);
            Ok(tonic::Response::new(Box::pin(s)))
        }
    }

    // Tower layer that swaps out the response body with TruncatedBody,
    // keeping the 200 status so the client enters the streaming path but
    // never receives a grpc-status trailer.
    #[derive(Clone)]
    struct TruncateLayer;

    impl<S> tower::Layer<S> for TruncateLayer {
        type Service = TruncateService<S>;
        fn layer(&self, inner: S) -> Self::Service {
            TruncateService(inner)
        }
    }

    #[derive(Clone)]
    struct TruncateService<S>(S);

    impl<S> tower::Service<http::Request<tonic::body::Body>> for TruncateService<S>
    where
        S: tower::Service<
                http::Request<tonic::body::Body>,
                Response = http::Response<tonic::body::Body>,
                Error = std::convert::Infallible,
            > + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
    {
        type Response = http::Response<tonic::body::Body>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            cx: &mut StdContext<'_>,
        ) -> StdPoll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
            let fut = self.0.call(req);
            Box::pin(async move {
                let resp = fut.await.unwrap();
                // Keep status 200, replace body with one that ends without trailers.
                let (parts, _original_body) = resp.into_parts();
                let truncated = tonic::body::Body::new(TruncatedBody);
                Ok(http::Response::from_parts(parts, truncated))
            })
        }
    }

    let svc = test_stream_server::TestStreamServer::new(Svc);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming =
        tonic::transport::server::TcpIncoming::from(listener).with_nodelay(Some(true));

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .layer(TruncateLayer)
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut client =
        test_stream_client::TestStreamClient::connect(format!("http://{addr}"))
            .await
            .unwrap();

    let mut stream = client
        .stream_call(InputStream {})
        .await
        .unwrap()
        .into_inner();

    // The stream must surface as an Internal error — NOT silently return None.
    let err = stream.message().await.unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::Internal,
        "expected Internal for missing grpc-status trailer, got: {:?}",
        err
    );
    assert!(
        err.message().contains("missing grpc-status trailer"),
        "unexpected error message: {}",
        err.message()
    );
}


