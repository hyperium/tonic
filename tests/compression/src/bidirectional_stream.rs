use super::*;
use http_body::Body;
use tonic::codec::CompressionEncoding;

util::parametrized_tests! {
    client_enabled_server_enabled,
    zstd: CompressionEncoding::Zstd,
    gzip: CompressionEncoding::Gzip,
}

#[allow(dead_code)]
async fn client_enabled_server_enabled(encoding: CompressionEncoding) {
    let (client, server) = tokio::io::duplex(UNCOMPRESSED_MIN_BODY_SIZE * 10);

    let svc = test_server::TestServer::new(Svc::default())
        .accept_compressed(encoding)
        .send_compressed(encoding);

    let request_bytes_counter = Arc::new(AtomicUsize::new(0));
    let response_bytes_counter = Arc::new(AtomicUsize::new(0));

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
        let response_bytes_counter = response_bytes_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .map_request(move |req| {
                            AssertRightEncoding::new(encoding).clone().call(req)
                        })
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
                .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
                .await
                .unwrap();
        }
    });

    let mut client = test_client::TestClient::new(mock_io_channel(client).await)
        .send_compressed(encoding)
        .accept_compressed(encoding);

    let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
    let stream = tokio_stream::iter(vec![SomeData { data: data.clone() }, SomeData { data }]);
    let req = Request::new(stream);

    let res = client
        .compress_input_output_bidirectional_stream(req)
        .await
        .unwrap();

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

    stream
        .next()
        .await
        .expect("stream empty")
        .expect("item was error");

    assert!(request_bytes_counter.load(SeqCst) < UNCOMPRESSED_MIN_BODY_SIZE);
    assert!(response_bytes_counter.load(SeqCst) < UNCOMPRESSED_MIN_BODY_SIZE);
}
