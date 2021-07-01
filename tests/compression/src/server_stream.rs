use super::*;
use tonic::Streaming;

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default()).send_gzip();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: bytes_sent_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(futures::stream::iter(vec![Ok::<_, std::io::Error>(
                    MockStream(server),
                )]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await).accept_gzip();

    let res = client.compress_output_server_stream(()).await.unwrap();

    assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");

    let mut stream: Streaming<SomeData> = res.into_inner();

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");
    assert!(dbg!(bytes_sent_counter.load(SeqCst)) < UNCOMPRESSED_MIN_BODY_SIZE);

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");
    assert!(dbg!(bytes_sent_counter.load(SeqCst)) < UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disabled_server_enabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default()).send_gzip();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: bytes_sent_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(futures::stream::iter(vec![Ok::<_, std::io::Error>(
                    MockStream(server),
                )]))
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
    assert!(dbg!(bytes_sent_counter.load(SeqCst)) > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled() {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default());

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: bytes_sent_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(futures::stream::iter(vec![Ok::<_, std::io::Error>(
                    MockStream(server),
                )]))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await).accept_gzip();

    let res = client.compress_output_server_stream(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let mut stream: Streaming<SomeData> = res.into_inner();

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");
    assert!(dbg!(bytes_sent_counter.load(SeqCst)) > UNCOMPRESSED_MIN_BODY_SIZE);
}
