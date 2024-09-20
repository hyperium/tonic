use std::pin::Pin;

use hyper_util::rt::TokioIo;
use integration_tests::{
    pb::{test1_client, test1_server, Input1, Output1},
    trace_init,
};
use tokio_stream::Stream;
use tonic::{
    transport::{Endpoint, Server},
    Code, Request, Response, Status,
};

#[test]
fn max_message_recv_size() {
    trace_init();

    // Server recv
    assert_server_recv_max_success(128);
    // 5 is the size of the gRPC header
    assert_server_recv_max_success((4 * 1024 * 1024) - 5);
    // 4mb is the max recv size
    assert_server_recv_max_failure(4 * 1024 * 1024);
    assert_server_recv_max_failure(4 * 1024 * 1024 + 1);
    assert_server_recv_max_failure(8 * 1024 * 1024);

    // Client recv
    assert_client_recv_max_success(128);
    // 5 is the size of the gRPC header
    assert_client_recv_max_success((4 * 1024 * 1024) - 5);
    // 4mb is the max recv size
    assert_client_recv_max_failure(4 * 1024 * 1024);
    assert_client_recv_max_failure(4 * 1024 * 1024 + 1);
    assert_client_recv_max_failure(8 * 1024 * 1024);

    // Custom limit settings
    assert_test_case(TestCase {
        // 5 is the size of the gRPC header
        server_blob_size: 1024 - 5,
        client_recv_max: Some(1024),
        ..Default::default()
    });
    assert_test_case(TestCase {
        server_blob_size: 1024,
        client_recv_max: Some(1024),
        expected_code: Some(Code::OutOfRange),
        ..Default::default()
    });

    assert_test_case(TestCase {
        // 5 is the size of the gRPC header
        client_blob_size: 1024 - 5,
        server_recv_max: Some(1024),
        ..Default::default()
    });
    assert_test_case(TestCase {
        client_blob_size: 1024,
        server_recv_max: Some(1024),
        expected_code: Some(Code::OutOfRange),
        ..Default::default()
    });
}

#[test]
fn max_message_send_size() {
    trace_init();

    // Check client send limit works
    assert_test_case(TestCase {
        client_blob_size: 4 * 1024 * 1024,
        server_recv_max: Some(usize::MAX),
        ..Default::default()
    });
    assert_test_case(TestCase {
        // 5 is the size of the gRPC header
        client_blob_size: 1024 - 5,
        server_recv_max: Some(usize::MAX),
        client_send_max: Some(1024),
        ..Default::default()
    });
    assert_test_case(TestCase {
        // 5 is the size of the gRPC header
        client_blob_size: 4 * 1024 * 1024,
        server_recv_max: Some(usize::MAX),
        // Set client send limit to 1024
        client_send_max: Some(1024),
        // TODO: This should return OutOfRange
        // https://github.com/hyperium/tonic/issues/1334
        expected_code: Some(Code::Internal),
        ..Default::default()
    });

    // Check server send limit works
    assert_test_case(TestCase {
        server_blob_size: 4 * 1024 * 1024,
        client_recv_max: Some(usize::MAX),
        ..Default::default()
    });
    assert_test_case(TestCase {
        // 5 is the gRPC header size
        server_blob_size: 1024 - 5,
        client_recv_max: Some(usize::MAX),
        // Set server send limit to 1024
        server_send_max: Some(1024),
        ..Default::default()
    });
    assert_test_case(TestCase {
        server_blob_size: 4 * 1024 * 1024,
        client_recv_max: Some(usize::MAX),
        // Set server send limit to 1024
        server_send_max: Some(1024),
        expected_code: Some(Code::OutOfRange),
        ..Default::default()
    });
}

#[tokio::test]
async fn response_stream_limit() {
    let client_blob = vec![0; 1];

    let (client, server) = tokio::io::duplex(1024);

    struct Svc;

    #[tonic::async_trait]
    impl test1_server::Test1 for Svc {
        async fn unary_call(&self, _req: Request<Input1>) -> Result<Response<Output1>, Status> {
            unimplemented!()
        }

        type StreamCallStream =
            Pin<Box<dyn Stream<Item = Result<Output1, Status>> + Send + 'static>>;

        async fn stream_call(
            &self,
            _req: Request<Input1>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            let blob = Output1 {
                buf: vec![0; 6877902],
            };
            let stream = tokio_stream::iter(vec![Ok(blob.clone()), Ok(blob.clone())]);

            Ok(Response::new(Box::pin(stream)))
        }
    }

    let svc = test1_server::Test1Server::new(Svc);

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
            .await
            .unwrap();
    });

    // Move client to an option so we can _move_ the inner value
    // on the first attempt to connect. All other attempts will fail.
    let mut client = Some(client);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let client = client.take();

            async move {
                if let Some(client) = client {
                    Ok(TokioIo::new(client))
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Client already taken",
                    ))
                }
            }
        }))
        .await
        .unwrap();

    let client = test1_client::Test1Client::new(channel);

    let mut client = client.max_decoding_message_size(6877902 + 5);

    let req = Request::new(Input1 {
        buf: client_blob.clone(),
    });

    let mut stream = client.stream_call(req).await.unwrap().into_inner();

    while let Some(_b) = stream.message().await.unwrap() {}
}

// Track caller doesn't work on async fn so we extract the async part
// into a sync version and assert the response there using track track_caller
// so that when this does panic it tells us which line in the test failed not
// where we placed the panic call.

#[track_caller]
fn assert_server_recv_max_success(size: usize) {
    let case = TestCase {
        client_blob_size: size,
        server_blob_size: 0,
        ..Default::default()
    };

    assert_test_case(case);
}

#[track_caller]
fn assert_server_recv_max_failure(size: usize) {
    let case = TestCase {
        client_blob_size: size,
        server_blob_size: 0,
        expected_code: Some(Code::OutOfRange),
        ..Default::default()
    };

    assert_test_case(case);
}

#[track_caller]
fn assert_client_recv_max_success(size: usize) {
    let case = TestCase {
        client_blob_size: 0,
        server_blob_size: size,
        ..Default::default()
    };

    assert_test_case(case);
}

#[track_caller]
fn assert_client_recv_max_failure(size: usize) {
    let case = TestCase {
        client_blob_size: 0,
        server_blob_size: size,
        expected_code: Some(Code::OutOfRange),
        ..Default::default()
    };

    assert_test_case(case);
}

#[track_caller]
fn assert_test_case(case: TestCase) {
    let res = max_message_run(&case);

    match (case.expected_code, res) {
        (Some(_), Ok(())) => panic!("Expected failure, but got success"),
        (Some(code), Err(status)) => {
            if status.code() != code {
                panic!(
                    "Expected failure, got failure but wrong code, got: {:?}",
                    status
                )
            }
        }

        (None, Err(status)) => panic!("Expected success, but got failure, got: {:?}", status),

        _ => (),
    }
}

#[derive(Default)]
struct TestCase {
    client_blob_size: usize,
    server_blob_size: usize,
    client_recv_max: Option<usize>,
    server_recv_max: Option<usize>,
    client_send_max: Option<usize>,
    server_send_max: Option<usize>,

    expected_code: Option<Code>,
}

#[tokio::main]
async fn max_message_run(case: &TestCase) -> Result<(), Status> {
    let client_blob = vec![0; case.client_blob_size];
    let server_blob = vec![0; case.server_blob_size];

    let (client, server) = tokio::io::duplex(1024);

    struct Svc(Vec<u8>);

    #[tonic::async_trait]
    impl test1_server::Test1 for Svc {
        async fn unary_call(&self, _req: Request<Input1>) -> Result<Response<Output1>, Status> {
            Ok(Response::new(Output1 {
                buf: self.0.clone(),
            }))
        }

        type StreamCallStream =
            Pin<Box<dyn Stream<Item = Result<Output1, Status>> + Send + 'static>>;

        async fn stream_call(
            &self,
            _req: Request<Input1>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            unimplemented!()
        }
    }

    let svc = test1_server::Test1Server::new(Svc(server_blob));

    let svc = if let Some(size) = case.server_recv_max {
        svc.max_decoding_message_size(size)
    } else {
        svc
    };

    let svc = if let Some(size) = case.server_send_max {
        svc.max_encoding_message_size(size)
    } else {
        svc
    };

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(server)))
            .await
            .unwrap();
    });

    // Move client to an option so we can _move_ the inner value
    // on the first attempt to connect. All other attempts will fail.
    let mut client = Some(client);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let client = client.take();

            async move {
                if let Some(client) = client {
                    Ok(TokioIo::new(client))
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Client already taken",
                    ))
                }
            }
        }))
        .await
        .unwrap();

    let client = test1_client::Test1Client::new(channel);

    let client = if let Some(size) = case.client_recv_max {
        client.max_decoding_message_size(size)
    } else {
        client
    };

    let mut client = if let Some(size) = case.client_send_max {
        client.max_encoding_message_size(size)
    } else {
        client
    };

    let req = Request::new(Input1 {
        buf: client_blob.clone(),
    });

    client.unary_call(req).await.map(|_| ())
}
