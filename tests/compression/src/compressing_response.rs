use super::*;
use tonic::codec::CompressionEncoding;

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

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
            assert_eq!(
                req.headers().get("grpc-accept-encoding").unwrap(),
                "gzip,identity"
            );
            self.0.call(req)
        }
    }

    let svc =
        test_server::TestServer::new(Svc::default()).send_compressed(CompressionEncoding::Gzip);

    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(layer_fn(AssertCorrectAcceptEncoding))
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: response_bytes_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .accept_compressed(CompressionEncoding::Gzip);

    for _ in 0..3 {
        let res = client.compress_output_unary(()).await.unwrap();
        assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");
        let bytes_sent = response_bytes_counter.load(SeqCst);
        assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default());

    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                // no compression enable on the server so responses should not be compressed
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: response_bytes_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .accept_compressed(CompressionEncoding::Gzip);

    let res = client.compress_output_unary(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

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

    let svc =
        test_server::TestServer::new(Svc::default()).send_compressed(CompressionEncoding::Gzip);

    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(layer_fn(AssertCorrectAcceptEncoding))
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: response_bytes_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await);

    let res = client.compress_output_unary(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn server_replying_with_unsupported_encoding() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc =
        test_server::TestServer::new(Svc::default()).send_compressed(CompressionEncoding::Gzip);

    fn add_weird_content_encoding<B>(mut response: http::Response<B>) -> http::Response<B> {
        response
            .headers_mut()
            .insert("grpc-encoding", "br".parse().unwrap());
        response
    }

    tokio::spawn(async move {
        Server::builder()
            .layer(
                ServiceBuilder::new()
                    .map_response(add_weird_content_encoding)
                    .into_inner(),
            )
            .add_service(svc)
            .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
            .await
            .unwrap();
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .accept_compressed(CompressionEncoding::Gzip);
    let status: Status = client.compress_output_unary(()).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert_eq!(
        status.message(),
        "Content is compressed with `br` which isn't supported"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_compression_on_single_response() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc {
        disable_compressing_on_response: true,
    })
    .send_compressed(CompressionEncoding::Gzip);

    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: response_bytes_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .accept_compressed(CompressionEncoding::Gzip);

    let res = client.compress_output_unary(()).await.unwrap();
    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");
    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_compression_on_response_but_keeping_compression_on_stream() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc {
        disable_compressing_on_response: true,
    })
    .send_compressed(CompressionEncoding::Gzip);

    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: response_bytes_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .accept_compressed(CompressionEncoding::Gzip);

    let res = client.compress_output_server_stream(()).await.unwrap();

    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");

    let mut stream: Streaming<SomeData> = res.into_inner();

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");
    assert!(response_bytes_counter.load(SeqCst) < UNCOMPRESSED_MIN_BODY_SIZE);

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");
    assert!(response_bytes_counter.load(SeqCst) < UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_compression_on_response_from_client_stream() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc {
        disable_compressing_on_response: true,
    })
    .send_compressed(CompressionEncoding::Gzip);

    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: response_bytes_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .accept_compressed(CompressionEncoding::Gzip);

    let stream = tokio_stream::iter(vec![]);
    let req = Request::new(Box::pin(stream));

    let res = client.compress_output_client_stream(req).await.unwrap();
    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");
    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}
