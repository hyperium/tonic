use super::*;
use http_body::Body;
use tonic::codec::{CompressionEncoding, EnabledCompressionEncodings};

util::parametrized_tests! {
    client_enabled_server_enabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
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
}

#[allow(dead_code)]
async fn client_enabled_server_enabled_multi_encoding(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default())
        .accept_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Zstd);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));

    fn assert_right_encoding<B>(req: http::Request<B>) -> http::Request<B> {
        let supported_encodings = ["gzip", "zstd"];
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
        _ => panic!("unexpected encoding {:?}", encoding),
    };
    assert_eq!(
        status.message(),
        format!(
            "Content is compressed with `{}` which isn't supported",
            expected
        )
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

#[test]
fn test_compression_priority() {
    let mut encodings = EnabledCompressionEncodings::default();
    encodings.enable(CompressionEncoding::Gzip);
    encodings.enable(CompressionEncoding::Zstd);

    assert_eq!(encodings.priority(CompressionEncoding::Gzip), Some(1));
    assert_eq!(encodings.priority(CompressionEncoding::Zstd), Some(0));

    encodings.enable(CompressionEncoding::Gzip);

    assert_eq!(encodings.priority(CompressionEncoding::Gzip), Some(0));
    assert_eq!(encodings.priority(CompressionEncoding::Zstd), Some(1));
}


fn build_accept_encoding_header(encodings: &str) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert("grpc-accept-encoding", encodings.parse().unwrap());
    headers
}

fn build_enabled_compression_settings(encodings: &[CompressionEncoding]) -> EnabledCompressionEncodings {
    let mut settings = EnabledCompressionEncodings::default();
    for encoding in encodings {
        settings.enable(*encoding);
    }
    settings
}

fn build_and_run_accept_encoding_header_test(
    encodings: &str,
    enabled_encodings: &[CompressionEncoding],
    expected: Option<CompressionEncoding>,
) {
    let header = build_accept_encoding_header(encodings);
    let compression = CompressionEncoding::from_accept_encoding_header(&header, build_enabled_compression_settings(enabled_encodings));
    assert_eq!(compression, expected);
}

#[test]
fn test_from_accept_encoding_header() {
    build_and_run_accept_encoding_header_test(
        "gzip",
        &[CompressionEncoding::Gzip],
        Some(CompressionEncoding::Gzip),
    );

    build_and_run_accept_encoding_header_test(
        "zstd",
        &[CompressionEncoding::Zstd],
        Some(CompressionEncoding::Zstd),
    );

    // Client provides ordering preferring gzip, but we prefer zstd
    build_and_run_accept_encoding_header_test(
        "gzip,zstd",
        &[CompressionEncoding::Zstd, CompressionEncoding::Gzip],
        Some(CompressionEncoding::Zstd),
    );

    // Client provides ordering preferring zstd, but we prefer gzip
    build_and_run_accept_encoding_header_test(
        "zstd,gzip",
        &[CompressionEncoding::Gzip, CompressionEncoding::Zstd],
        Some(CompressionEncoding::Gzip),
    );

    // Client provides ordering preferring gzip, and we also prefer gzip
    build_and_run_accept_encoding_header_test(
        "gzip,zstd",
        &[CompressionEncoding::Gzip, CompressionEncoding::Zstd],
        Some(CompressionEncoding::Gzip),
    );

    // Client provides two, but we don't support any
    build_and_run_accept_encoding_header_test(
        "gzip,zstd",
        &[],
        None,
    );

    // Client provides gzip, but we only support zstd
    build_and_run_accept_encoding_header_test(
        "gzip",
        &[CompressionEncoding::Zstd],
        None,
    );
}
