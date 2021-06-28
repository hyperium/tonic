use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_enabled() {
    #[derive(Clone, Copy)]
    struct AssertCorrectAcceptEncoding<S>(S);

    impl<S, B> Service<http::Request<B>> for AssertCorrectAcceptEncoding<S>
    where
        S: Service<http::Request<B>>,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = S::Future;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            assert_eq!(
                req.headers().get("grpc-accept-encoding").unwrap(),
                "gzip,identity"
            );
            self.0.call(req)
        }
    }

    let svc = test_server::TestServer::new(Svc).send_gzip();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(layer_fn(AssertCorrectAcceptEncoding))
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

    let mut client = test_client::TestClient::new(channel).accept_gzip();

    for _ in 0..3 {
        let res = client.compress_output_unary(()).await.unwrap();
        assert_eq!(res.metadata().get("grpc-encoding").unwrap(), "gzip");
        let bytes_sent = bytes_sent_counter.load(Relaxed);
        assert!(bytes_sent < UNCOMPRESSED_MIN_BODY_SIZE);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_enabled_server_disabled() {
    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                // no compression enable on the server so responses should not be compressed
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
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        }
    });

    let channel = Channel::builder(format!("http://{}", addr).parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel).accept_gzip();

    let res = client.compress_output_unary(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = bytes_sent_counter.load(Relaxed);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disabled() {
    #[derive(Clone, Copy)]
    struct AssertCorrectAcceptEncoding<S>(S);

    impl<S, B> Service<http::Request<B>> for AssertCorrectAcceptEncoding<S>
    where
        S: Service<http::Request<B>>,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = S::Future;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            assert!(req.headers().get("grpc-accept-encoding").is_none());
            self.0.call(req)
        }
    }

    let svc = test_server::TestServer::new(Svc).send_gzip();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let bytes_sent_counter = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let bytes_sent_counter = bytes_sent_counter.clone();
        async move {
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(layer_fn(AssertCorrectAcceptEncoding))
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

    let mut client = test_client::TestClient::new(channel);

    let res = client.compress_output_unary(()).await.unwrap();

    assert!(res.metadata().get("grpc-encoding").is_none());

    let bytes_sent = bytes_sent_counter.load(Relaxed);
    assert!(bytes_sent > UNCOMPRESSED_MIN_BODY_SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn server_replying_with_unsupported_encoding() {
    let svc = test_server::TestServer::new(Svc).send_gzip();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    fn add_weird_content_encoding<B>(mut response: http::Response<B>) -> http::Response<B> {
        response
            .headers_mut()
            .insert("grpc-encoding", "br".parse().unwrap());
        response
    }

    tokio::spawn(async move {
        Server::builder()
            .layer(
                ServiceBuilder::new()
                    .map_response(add_weird_content_encoding)
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

    let mut client = test_client::TestClient::new(channel).accept_gzip();
    let status: Status = client.compress_output_unary(()).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert_eq!(
        status.message(),
        "Content is compressed with `br` which isn't supported"
    );
}
