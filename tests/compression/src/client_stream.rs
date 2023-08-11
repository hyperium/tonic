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
                        .map_request(assert_right_encoding)
                        .layer(measure_request_body_size_layer(
                            request_bytes_counter.clone(),
                        ))
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

    let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
    let stream = tokio_stream::iter(vec![SomeData { data: data.clone() }, SomeData { data }]);
    let req = Request::new(Box::pin(stream));

    client.compress_input_client_stream(req).await.unwrap();

    let bytes_sent = request_bytes_counter.load(SeqCst);
    assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disabled_server_enabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc =
        test_server::TestServer::new(Svc::default()).accept_compressed(CompressionEncoding::Gzip);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));

    fn assert_right_encoding<B>(req: http::Request<B>) -> http::Request<B> {
        assert!(req.headers().get("grpc-encoding").is_none());
        req
    }

    tokio::spawn({
        let request_bytes_counter = request_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .map_request(assert_right_encoding)
                        .layer(measure_request_body_size_layer(
                            request_bytes_counter.clone(),
                        ))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::iter(vec![Ok::<_, std::io::Error>(server)]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await);

    let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
    let stream = tokio_stream::iter(vec![SomeData { data: data.clone() }, SomeData { data }]);
    let req = Request::new(Box::pin(stream));

    client.compress_input_client_stream(req).await.unwrap();

    let bytes_sent = request_bytes_counter.load(SeqCst);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
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

    let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
    let stream = tokio_stream::iter(vec![SomeData { data: data.clone() }, SomeData { data }]);
    let req = Request::new(Box::pin(stream));

    let status = client.compress_input_client_stream(req).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert_eq!(
        status.message(),
        "Content is compressed with `gzip` which isn't supported"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn compressing_response_from_client_stream() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc =
        test_server::TestServer::new(Svc::default()).send_compressed(CompressionEncoding::Gzip);

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
    assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
}
