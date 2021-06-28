use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
    let svc = test_server::TestServer::new(Svc).accept_gzip().send_gzip();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    fn assert_right_encoding<B>(req: http::Request<B>) -> http::Request<B> {
        assert_eq!(req.headers().get("grpc-encoding").unwrap(), "gzip");
        req
    }

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .map_request(assert_right_encoding)
                        .layer(measure_request_body_size_layer(bytes_sent_counter.clone()))
                        .layer(MapResponseBodyLayer::new(move |body| {
                            util::CountBytesBody {
                                inner: body,
                                counter: bytes_sent_counter.clone(),
                            }
                        }))
                        .into_inner(),
                )
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        }
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel)
        .send_gzip()
        .accept_gzip();

    let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
    let stream = futures::stream::iter(vec![SomeData { data: data.clone() }, SomeData { data }]);
    let req = Request::new(Box::pin(stream));

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

    let bytes_sent = bytes_sent_counter.load(Relaxed);
    assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
}
