use super::*;
use http_body::Body;
use tonic::codec::CompressionEncoding;

util::parametrized_tests! {
    client_enabled_server_enabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
    lz4: CompressionEncoding::Lz4,
    snappy: CompressionEncoding::Snappy,
    deflate: CompressionEncoding::Deflate,
}

#[allow(dead_code)]
async fn client_enabled_server_enabled(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default()).accept_compressed(encoding);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));

    #[derive(Clone)]
    pub struct AssertRightEncoding {
        encoding: CompressionEncoding,
    }

    #[allow(dead_code)]
    impl AssertRightEncoding {
        pub fn new(encoding: CompressionEncoding) -> Self {
            Self { encoding }
        }

        pub fn call<B: Body>(self, req: http::Request<B>) -> http::Request<B> {
            let expected = match self.encoding {
                CompressionEncoding::Gzip => "gzip",
                CompressionEncoding::Zstd => "zstd",
                CompressionEncoding::Lz4 => "lz4",
                CompressionEncoding::Snappy => "snappy",
                CompressionEncoding::Deflate => "deflate",
                _ => panic!("unexpected encoding {:?}", self.encoding),
            };
            assert_eq!(req.headers().get("grpc-encoding").unwrap(), expected);

            req
        }
    }

    tokio::spawn({
        let request_bytes_counter = request_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(
                            ServiceBuilder::new()
                                .map_request(move |req| {
                                    AssertRightEncoding::new(encoding).clone().call(req)
                                })
                                .layer(measure_request_body_size_layer(request_bytes_counter))
                                .into_inner(),
                        )
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).send_compressed(encoding);

    for _ in 0..3 {
        client
            .compress_input_unary(SomeData {
                data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
            })
            .await
            .unwrap();
        let bytes_sent = request_bytes_counter.load(SeqCst);
        assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
    }
}

util::parametrized_tests! {
    client_enabled_server_enabled_multi_encoding,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
    lz4: CompressionEncoding::Lz4,
    snappy: CompressionEncoding::Snappy,
    deflate: CompressionEncoding::Deflate,
}

#[allow(dead_code)]
async fn client_enabled_server_enabled_multi_encoding(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default())
        .accept_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Zstd)
        .accept_compressed(CompressionEncoding::Lz4)
        .accept_compressed(CompressionEncoding::Snappy)
        .accept_compressed(CompressionEncoding::Deflate);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));

    fn assert_right_encoding<B>(req: http::Request<B>) -> http::Request<B> {
        let supported_encodings = ["gzip", "zstd", "lz4", "snappy", "deflate"];
        let req_encoding = req.headers().get("grpc-encoding").unwrap();
        assert!(supported_encodings.iter().any(|e| e == req_encoding));

        req
    }

    tokio::spawn({
        let request_bytes_counter = request_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(
                            ServiceBuilder::new()
                                .map_request(assert_right_encoding)
                                .layer(measure_request_body_size_layer(request_bytes_counter))
                                .into_inner(),
                        )
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).send_compressed(encoding);

    for _ in 0..3 {
        client
            .compress_input_unary(SomeData {
                data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
            })
            .await
            .unwrap();
        let bytes_sent = request_bytes_counter.load(SeqCst);
        assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
    }
}

parametrized_tests! {
    client_enabled_server_disabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
    lz4: CompressionEncoding::Lz4,
    snappy: CompressionEncoding::Snappy,
    deflate: CompressionEncoding::Deflate,
}

#[allow(dead_code)]
async fn client_enabled_server_disabled(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default());

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
            .await
            .unwrap();
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).send_compressed(encoding);

    let status = client
        .compress_input_unary(SomeData {
            data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
        })
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    let expected = match encoding {
        CompressionEncoding::Gzip => "gzip",
        CompressionEncoding::Zstd => "zstd",
        CompressionEncoding::Lz4 => "lz4",
        CompressionEncoding::Snappy => "snappy",
        CompressionEncoding::Deflate => "deflate",
        _ => panic!("unexpected encoding {encoding:?}"),
    };
    assert_eq!(
        status.message(),
        format!("Content is compressed with `{expected}` which isn't supported")
    );

    assert_eq!(
        status.metadata().get("grpc-accept-encoding").unwrap(),
        "identity"
    );
}
parametrized_tests! {
    client_mark_compressed_without_header_server_enabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
    lz4: CompressionEncoding::Lz4,
    snappy: CompressionEncoding::Snappy,
    deflate: CompressionEncoding::Deflate,
}

#[allow(dead_code)]
async fn client_mark_compressed_without_header_server_enabled(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default()).accept_compressed(encoding);

    tokio::spawn({
        async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::with_interceptor(
        mock_io_channel(client).await,
        move |mut req: Request<()>| {
            req.metadata_mut().remove("grpc-encoding");
            Ok(req)
        },
    )
    .send_compressed(CompressionEncoding::Gzip);

    let status = client
        .compress_input_unary(SomeData {
            data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
        })
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::Internal);
    assert_eq!(
        status.message(),
        "protocol error: received message with compressed-flag but no grpc-encoding was specified"
    );
}

util::parametrized_tests! {
    limit_decoded_message_size,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
    lz4: CompressionEncoding::Lz4,
    snappy: CompressionEncoding::Snappy,
    deflate: CompressionEncoding::Deflate,
}

#[cfg(test)]
async fn limit_decoded_message_size(encoding: CompressionEncoding) {
    use prost::Message;

    let under_limit_request = SomeData {
        data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
    };
    let limit = under_limit_request.encoded_len();
    let over_limit_request = SomeData {
        data: [0_u8; 1 + UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
    };

    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default())
        .accept_compressed(encoding)
        .max_decoding_message_size(limit);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let request_bytes_counter = request_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(
                            ServiceBuilder::new()
                                .layer(measure_request_body_size_layer(request_bytes_counter))
                                .into_inner(),
                        )
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client =
        test_client::TestClient::new(mock_io_channel(client).await).send_compressed(encoding);

    for _ in 0..3 {
        // compressed messages that are under or exactly at the limit are successful.
        client
            .compress_input_unary(under_limit_request.clone())
            .await
            .unwrap();
        let bytes_sent = request_bytes_counter.load(SeqCst);
        assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);

        // compressed messages that are over the limit are fail with resource exhausted
        let status = client
            .compress_input_unary(over_limit_request.clone())
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    }
}
