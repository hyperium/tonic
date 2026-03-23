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
// Unknown, not silently treated as a clean end-of-stream.
//
// We simulate the exact scenario described in `infer_grpc_status`: a proxy or
// load-balancer that issues RST_STREAM(NO_ERROR) — e.g. an Envoy timeout —
// without ever sending a grpc-status trailer.  hyper converts
// RST_STREAM(NO_ERROR) into a clean end-of-stream (Poll::Ready(None)), so
// tonic's only signal is the absence of the trailer on an HTTP 200 response.
//
// We use the `h2` crate directly to drive a raw HTTP/2 server that:
//   1. Completes the HTTP/2 handshake
//   2. Sends HTTP 200 + `content-type: application/grpc` response headers
//   3. Sends RST_STREAM with reason NO_ERROR (no data, no trailers)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_grpc_status_trailer_is_unknown_error() {
    integration_tests::trace_init();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Oneshot channel: client signals the server once `stream_call().await`
    // has returned, i.e. the response HEADERS have been received and the
    // client is about to enter the body-reading loop.  The server delays
    // `send_reset` until it receives this signal so that:
    //
    //   1. RST_STREAM is queued only AFTER the HEADERS frame has already
    //      been flushed over the wire (h2's `send_reset` calls `clear_queue`
    //      which would otherwise discard any still-pending DATA frames, but
    //      here there are none — only the already-flushed HEADERS matter).
    //
    //   2. The client is inside `Incoming::poll_data` (or about to enter it)
    //      when RST_STREAM arrives.  That is exactly the branch where hyper
    //      converts `Reset(NO_ERROR)` to `Poll::Ready(None)` (a clean
    //      end-of-stream) rather than surfacing it as an error:
    //
    //        Some(Err(e)) => match e.reason() {
    //            Some(h2::Reason::NO_ERROR) | Some(h2::Reason::CANCEL)
    //                => Poll::Ready(None),   // ← taken here
    //            _   => Poll::Ready(Some(Err(...))),
    //        }
    //
    //      tonic's decoder sees None with an empty buffer, calls
    //      `infer_grpc_status(trailers=None, status=200)`, and returns
    //      `Code::Unknown` because it is not able to observe the
    //      RST_STREAM(NO_ERROR) at this time and only sees a stream end
    //      successfully but without trailers containing grpc-status.
    //      TODO: this should expect Code::Internal instead.

    let (headers_acked_tx, headers_acked_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut conn = h2::server::handshake(socket).await.unwrap();

        // Accept the single gRPC request and send HTTP 200 + grpc headers.
        let mut send_stream = if let Some(Ok((_request, mut respond))) = conn.accept().await {
            let response = http::Response::builder()
                .status(http::StatusCode::OK)
                .header("content-type", "application/grpc")
                .body(())
                .unwrap();
            respond.send_response(response, false).unwrap()
        } else {
            return;
        };

        // Wait until the client confirms it received the HEADERS, then send
        // RST_STREAM(NO_ERROR) with no grpc-status trailer.
        tokio::spawn(async move {
            headers_acked_rx.await.ok();
            send_stream.send_reset(h2::Reason::NO_ERROR);
        });

        // Drive the connection until it closes, flushing HEADERS and (later) RST_STREAM.
        while conn.accept().await.is_some() {}
    });

    let mut client = test_stream_client::TestStreamClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    // Wait for response HEADERS (stream_call completes the HTTP/2 header phase).
    // Then immediately tell the server we are about to read the body.
    let response = client.stream_call(InputStream {}).await.unwrap();
    headers_acked_tx.send(()).ok();
    let mut stream = response.into_inner();

    // The stream must surface as an Unknown error — NOT silently return None.
    let err = stream.message().await.unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::Unknown,
        "expected Unknown for RST_STREAM(NO_ERROR) without grpc-status trailer, got: {:?}",
        err
    );
    assert!(
        err.message().contains("missing grpc-status trailer"),
        "unexpected error message: {}",
        err.message()
    );
}
