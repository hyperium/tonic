use super::*;
use tonic::codec::CompressionEncoding;

util::parametrized_tests! {
    client_enabled_server_enabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn client_enabled_server_enabled(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    #[derive(Clone, Copy)]
    struct AssertCorrectAcceptEncoding<S> {
        service: S,
        encoding: CompressionEncoding,
    }

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
            self.service.poll_ready(cx)
        }

        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let expected = match self.encoding {
                CompressionEncoding::Gzip => "gzip",
                CompressionEncoding::Zstd => "zstd",
                _ => panic!("unexpected encoding {:?}", self.encoding),
            };
            assert_eq!(
                req.headers()
                    .get("grpc-accept-encoding")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("{},identity", expected)
            );
            self.service.call(req)
        }
    }

    let svc = test_server::TestServer::new(Svc::default()).send_compressed(encoding);

    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(layer_fn(|service| AssertCorrectAcceptEncoding {
                            service,
                            encoding,
                        }))
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: response_bytes_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).accept_compressed(encoding);

    let expected = match encoding {
        CompressionEncoding::Gzip => "gzip",
        CompressionEncoding::Zstd => "zstd",
        _ => panic!("unexpected encoding {:?}", encoding),
    };

    for _ in 0..3 {
        let res = client.compress_output_unary(()).await.unwrap();
        assert_eq!(res.metadata().get("grpc-encoding").unwrap(), expected);
        let bytes_sent = response_bytes_counter.load(SeqCst);
        assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
    }
}

util::parametrized_tests! {
    client_enabled_server_disabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn client_enabled_server_disabled(encoding: CompressionEncoding) {
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

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).accept_compressed(encoding);

    let res = client.compress_output_unary(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled_multi_encoding() {
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
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .accept_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Zstd);

    let res = client.compress_output_unary(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

util::parametrized_tests! {
    client_disabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn client_disabled(encoding: CompressionEncoding) {
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

    let svc = test_server::TestServer::new(Svc::default()).send_compressed(encoding);

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
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
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

util::parametrized_tests! {
    server_replying_with_unsupported_encoding,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn server_replying_with_unsupported_encoding(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default()).send_compressed(encoding);

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
            .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
            .await
            .unwrap();
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).accept_compressed(encoding);
    let status: Status = client.compress_output_unary(()).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert_eq!(
        status.message(),
        "Content is compressed with `br` which isn't supported"
    );
}

util::parametrized_tests! {
    disabling_compression_on_single_response,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn disabling_compression_on_single_response(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc {
        disable_compressing_on_response: true,
    })
    .send_compressed(encoding);

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
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).accept_compressed(encoding);

    let res = client.compress_output_unary(()).await.unwrap();

    let expected = match encoding {
        CompressionEncoding::Gzip => "gzip",
        CompressionEncoding::Zstd => "zstd",
        _ => panic!("unexpected encoding {:?}", encoding),
    };
    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), expected);

    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

util::parametrized_tests! {
    disabling_compression_on_response_but_keeping_compression_on_stream,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn disabling_compression_on_response_but_keeping_compression_on_stream(
    encoding: CompressionEncoding,
) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc {
        disable_compressing_on_response: true,
    })
    .send_compressed(encoding);

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
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).accept_compressed(encoding);

    let res = client.compress_output_server_stream(()).await.unwrap();

    let expected = match encoding {
        CompressionEncoding::Gzip => "gzip",
        CompressionEncoding::Zstd => "zstd",
        _ => panic!("unexpected encoding {:?}", encoding),
    };
    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), expected);

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

util::parametrized_tests! {
    disabling_compression_on_response_from_client_stream,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn disabling_compression_on_response_from_client_stream(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc {
        disable_compressing_on_response: true,
    })
    .send_compressed(encoding);

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
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).accept_compressed(encoding);

    let req = Request::new(Box::pin(tokio_stream::empty()));

    let res = client.compress_output_client_stream(req).await.unwrap();

    let expected = match encoding {
        CompressionEncoding::Gzip => "gzip",
        CompressionEncoding::Zstd => "zstd",
        _ => panic!("unexpected encoding {:?}", encoding),
    };
    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), expected);
    let bytes_sent = response_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}
