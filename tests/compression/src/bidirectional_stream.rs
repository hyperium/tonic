use super::*;
use tonic::codec::CompressionEncoding;

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default())
        .accept_compressed(CompressionEncoding::Gzip)
        .send_compressed(CompressionEncoding::Gzip);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));
    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

    fn assert_right_encoding<B>(req: http::Request<B>) -> http::Request<B> {
        assert_eq!(req.headers().get("grpc-encoding").unwrap(), "gzip");
        req
    }

    tokio::spawn({
        let request_bytes_counter = request_bytes_counter.clone();
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .map_request(assert_right_encoding)
                        .layer(measure_request_body_size_layer(
                            request_bytes_counter.clone(),
                        ))
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
        .send_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Gzip);

    let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
    let stream = tokio_stream::iter(vec![SomeData { data: data.clone() }, SomeData { data }]);
    let req = Request::new(stream);

    let res = client
        .compress_input_output_bidirectional_stream(req)
        .await
        .unwrap();

    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");

    let mut stream: Streaming<SomeData> = res.into_inner();

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");

    assert!(request_bytes_counter.load(SeqCst) < UNCOMPRESSED_MIN_BODY_SIZE);
    assert!(response_bytes_counter.load(SeqCst) < UNCOMPRESSED_MIN_BODY_SIZE);
}
