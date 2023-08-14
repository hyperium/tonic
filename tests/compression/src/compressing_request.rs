use super::*;
use http_body::Body as _;
use tonic::codec::CompressionEncoding;

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc =
        test_server::TestServer::new(Svc::default()).accept_compressed(CompressionEncoding::Gzip);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));

    fn assert_right_encoding<B>(req: http::Request<B>) -> http::Request<B> {
        assert_eq!(req.headers().get("grpc-encoding").unwrap(), "gzip");
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
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .send_compressed(CompressionEncoding::Gzip);

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

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default());

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
            .await
            .unwrap();
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .send_compressed(CompressionEncoding::Gzip);

    let status = client
        .compress_input_unary(SomeData {
            data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
        })
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert_eq!(
        status.message(),
        "Content is compressed with `gzip` which isn't supported"
    );

    assert_eq!(
        status.metadata().get("grpc-accept-encoding").unwrap(),
        "identity"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn client_mark_compressed_without_header_server_enabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc =
        test_server::TestServer::new(Svc::default()).accept_compressed(CompressionEncoding::Gzip);

    tokio::spawn({
        async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
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
