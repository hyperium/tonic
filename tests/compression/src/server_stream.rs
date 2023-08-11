use super::*;
use tonic::codec::CompressionEncoding;
use tonic::Streaming;

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
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
async fn client_disabled_server_enabled() {
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

    let mut client = test_client::TestClient::new(mock_io_channel(client).await);

    let res = client.compress_output_server_stream(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let mut stream: Streaming<SomeData> = res.into_inner();

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");
    assert!(response_bytes_counter.load(SeqCst) > UNCOMPRESSED_MIN_BODY_SIZE);
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

    assert!(res.metadata().get("grpc-encoding").is_none());

    let mut stream: Streaming<SomeData> = res.into_inner();

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");
    assert!(response_bytes_counter.load(SeqCst) > UNCOMPRESSED_MIN_BODY_SIZE);
}
