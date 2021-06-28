use super::*;
use http_body::Body as _;

// TODO(david): send_gzip on channel, but disabling compression of a message

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
    let svc = test_server::TestServer::new(Svc).accept_gzip();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    let measure_request_body_size_layer = {
        let bytes_sent_counter = bytes_sent_counter.clone();
        MapRequestBodyLayer::new(move |mut body: hyper::Body| {
            let (mut tx, new_body) = hyper::Body::channel();

            let bytes_sent_counter = bytes_sent_counter.clone();
            tokio::spawn(async move {
                while let Some(chunk) = body.data().await {
                    let chunk = chunk.unwrap();
                    bytes_sent_counter.fetch_add(chunk.len(), Relaxed);
                    tx.send_data(chunk).await.unwrap();
                }

                if let Some(trailers) = body.trailers().await.unwrap() {
                    tx.send_trailers(trailers).await.unwrap();
                }
            });

            new_body
        })
    };

    fn assert_right_encoding<B>(req: http::Request<B>) -> http::Request<B> {
        assert_eq!(req.headers().get("grpc-encoding").unwrap(), "gzip");
        req
    }

    tokio::spawn(async move {
        Server::builder()
            .layer(
                ServiceBuilder::new()
                    .layer(
                        ServiceBuilder::new()
                            .map_request(assert_right_encoding)
                            .layer(measure_request_body_size_layer)
                            .into_inner(),
                    )
                    .into_inner(),
            )
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel).send_gzip();

    client
        .compress_input(SomeData {
            data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
        })
        .await
        .unwrap();

    let bytes_sent = bytes_sent_counter.load(Relaxed);
    dbg!(&bytes_sent);
    assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled() {
    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel).send_gzip();

    let status = client
        .compress_input(SomeData {
            data: [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec(),
        })
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert_eq!(
        status.message(),
        "Request is compressed with `gzip` which the server doesn't support"
    );
}
