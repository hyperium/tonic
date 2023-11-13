use crate::{
    pb::test_service_client::*, pb::unimplemented_service_client::*, pb::*, test_assert,
    TestAssertion,
};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use tonic::{metadata::MetadataValue, Code, Request, Response, Status};

pub type TestClient = TestServiceClient<Channel>;
pub type UnimplementedClient = UnimplementedServiceClient<Channel>;

const LARGE_REQ_SIZE: usize = 271_828;
const LARGE_RSP_SIZE: i32 = 314_159;
const REQUEST_LENGTHS: &[i32] = &[27182, 8, 1828, 45904];
const RESPONSE_LENGTHS: &[i32] = &[31415, 9, 2653, 58979];
const TEST_STATUS_MESSAGE: &str = "test status message";
const SPECIAL_TEST_STATUS_MESSAGE: &str =
    "\t\ntest with whitespace\r\nand Unicode BMP ☺ and non-BMP 😈\t\n";

pub async fn empty_unary(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let result = client.empty_call(Request::new(Empty {})).await;

    assertions.push(test_assert!(
        "call must be successful",
        result.is_ok(),
        format!("result={:?}", result)
    ));

    if let Ok(response) = result {
        let body = response.into_inner();
        assertions.push(test_assert!(
            "body must not be null",
            body == Empty {},
            format!("body={:?}", body)
        ));
    }
}

pub async fn large_unary(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    use std::mem;
    let payload = crate::client_payload(LARGE_REQ_SIZE);
    let req = SimpleRequest {
        response_type: PayloadType::Compressable as i32,
        response_size: LARGE_RSP_SIZE,
        payload: Some(payload),
        ..Default::default()
    };

    let result = client.unary_call(Request::new(req)).await;

    assertions.push(test_assert!(
        "call must be successful",
        result.is_ok(),
        format!("result={:?}", result)
    ));

    if let Ok(response) = result {
        let body = response.into_inner();
        let payload_len = body.payload.as_ref().map(|p| p.body.len()).unwrap_or(0);

        assertions.push(test_assert!(
            "body must be 314159 bytes",
            payload_len == LARGE_RSP_SIZE as usize,
            format!("mem::size_of_val(&body)={:?}", mem::size_of_val(&body))
        ));
    }
}

// pub async fn cachable_unary(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
//     let payload = Payload {
//         r#type: PayloadType::Compressable as i32,
//         body: format!("{:?}", std::time::Instant::now()).into_bytes(),
//     };
//     let req = SimpleRequest {
//         response_type: PayloadType::Compressable as i32,
//         payload: Some(payload),
//         ..Default::default()
//     };

//     client.
// }

pub async fn client_streaming(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let requests = REQUEST_LENGTHS.iter().map(|len| StreamingInputCallRequest {
        payload: Some(crate::client_payload(*len as usize)),
        ..Default::default()
    });

    let stream = tokio_stream::iter(requests);

    let result = client.streaming_input_call(Request::new(stream)).await;

    assertions.push(test_assert!(
        "call must be successful",
        result.is_ok(),
        format!("result={:?}", result)
    ));

    if let Ok(response) = result {
        let body = response.into_inner();

        assertions.push(test_assert!(
            "aggregated payload size must be 74922 bytes",
            body.aggregated_payload_size == 74922,
            format!("aggregated_payload_size={:?}", body.aggregated_payload_size)
        ));
    }
}

pub async fn server_streaming(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let req = StreamingOutputCallRequest {
        response_parameters: RESPONSE_LENGTHS
            .iter()
            .map(|len| ResponseParameters::with_size(*len))
            .collect(),
        ..Default::default()
    };
    let req = Request::new(req);

    let result = client.streaming_output_call(req).await;

    assertions.push(test_assert!(
        "call must be successful",
        result.is_ok(),
        format!("result={:?}", result)
    ));

    if let Ok(response) = result {
        let responses = response
            .into_inner()
            .filter_map(|m| m.ok())
            .collect::<Vec<_>>()
            .await;
        let actual_response_lengths = crate::response_lengths(&responses);
        let asserts = vec![
            test_assert!(
                "there should be four responses",
                responses.len() == 4,
                format!("responses.len()={:?}", responses.len())
            ),
            test_assert!(
                "the response payload sizes should match input",
                RESPONSE_LENGTHS == actual_response_lengths.as_slice(),
                format!("{:?}={:?}", RESPONSE_LENGTHS, actual_response_lengths)
            ),
        ];

        assertions.extend(asserts);
    }
}

pub async fn ping_pong(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(make_ping_pong_request(0)).unwrap();

    let result = client
        .full_duplex_call(Request::new(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
        .await;

    assertions.push(test_assert!(
        "call must be successful",
        result.is_ok(),
        format!("result={:?}", result)
    ));

    if let Ok(mut response) = result.map(Response::into_inner) {
        let mut responses = Vec::new();

        loop {
            match response.next().await {
                Some(result) => {
                    responses.push(result.unwrap());
                    if responses.len() == REQUEST_LENGTHS.len() {
                        drop(tx);
                        break;
                    } else {
                        tx.send(make_ping_pong_request(responses.len())).unwrap();
                    }
                }
                None => {
                    assertions.push(TestAssertion::Failed {
                        description:
                            "server should keep the stream open until the client closes it",
                        expression: "Stream terminated unexpectedly early",
                        why: None,
                    });
                    break;
                }
            }
        }

        let actual_response_lengths = crate::response_lengths(&responses);
        assertions.push(test_assert!(
            "there should be four responses",
            responses.len() == RESPONSE_LENGTHS.len(),
            format!("{:?}={:?}", responses.len(), RESPONSE_LENGTHS.len())
        ));
        assertions.push(test_assert!(
            "the response payload sizes should match input",
            RESPONSE_LENGTHS == actual_response_lengths.as_slice(),
            format!("{:?}={:?}", RESPONSE_LENGTHS, actual_response_lengths)
        ));
    }
}

pub async fn empty_stream(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let stream = tokio_stream::empty();
    let result = client.full_duplex_call(Request::new(stream)).await;

    assertions.push(test_assert!(
        "call must be successful",
        result.is_ok(),
        format!("result={:?}", result)
    ));

    if let Ok(response) = result.map(Response::into_inner) {
        let responses = response.collect::<Vec<_>>().await;

        assertions.push(test_assert!(
            "there should be no responses",
            responses.is_empty(),
            format!("responses.len()={:?}", responses.len())
        ));
    }
}

pub async fn status_code_and_message(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    fn validate_response<T>(result: Result<T, Status>, assertions: &mut Vec<TestAssertion>)
    where
        T: std::fmt::Debug,
    {
        assertions.push(test_assert!(
            "call must fail with unknown status code",
            match &result {
                Err(status) => status.code() == Code::Unknown,
                _ => false,
            },
            format!("result={:?}", result)
        ));

        assertions.push(test_assert!(
            "call must respsond with expected status message",
            match &result {
                Err(status) => status.message() == TEST_STATUS_MESSAGE,
                _ => false,
            },
            format!("result={:?}", result)
        ));
    }

    let simple_req = SimpleRequest {
        response_status: Some(EchoStatus {
            code: 2,
            message: TEST_STATUS_MESSAGE.to_string(),
        }),
        ..Default::default()
    };

    let duplex_req = StreamingOutputCallRequest {
        response_status: Some(EchoStatus {
            code: 2,
            message: TEST_STATUS_MESSAGE.to_string(),
        }),
        ..Default::default()
    };

    let result = client.unary_call(Request::new(simple_req)).await;
    validate_response(result, assertions);

    let stream = tokio_stream::once(duplex_req);
    let result = match client.full_duplex_call(Request::new(stream)).await {
        Ok(response) => {
            let stream = response.into_inner();
            let responses = stream.collect::<Vec<_>>().await;
            Ok(responses)
        }
        Err(e) => Err(e),
    };

    validate_response(result, assertions);
}

pub async fn special_status_message(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let req = SimpleRequest {
        response_status: Some(EchoStatus {
            code: 2,
            message: SPECIAL_TEST_STATUS_MESSAGE.to_string(),
        }),
        ..Default::default()
    };

    let result = client.unary_call(Request::new(req)).await;

    assertions.push(test_assert!(
        "call must fail with unknown status code",
        match &result {
            Err(status) => status.code() == Code::Unknown,
            _ => false,
        },
        format!("result={:?}", result)
    ));

    assertions.push(test_assert!(
        "call must respsond with expected status message",
        match &result {
            Err(status) => status.message() == SPECIAL_TEST_STATUS_MESSAGE,
            _ => false,
        },
        format!("result={:?}", result)
    ));
}

pub async fn unimplemented_method(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let result = client.unimplemented_call(Request::new(Empty {})).await;
    assertions.push(test_assert!(
        "call must fail with unimplemented status code",
        match &result {
            Err(status) => status.code() == Code::Unimplemented,
            _ => false,
        },
        format!("result={:?}", result)
    ));
}

pub async fn unimplemented_service(
    client: &mut UnimplementedClient,
    assertions: &mut Vec<TestAssertion>,
) {
    let result = client.unimplemented_call(Request::new(Empty {})).await;
    assertions.push(test_assert!(
        "call must fail with unimplemented status code",
        match &result {
            Err(status) => status.code() == Code::Unimplemented,
            _ => false,
        },
        format!("result={:?}", result)
    ));
}

pub async fn custom_metadata(client: &mut TestClient, assertions: &mut Vec<TestAssertion>) {
    let key1 = "x-grpc-test-echo-initial";
    let value1: MetadataValue<_> = "test_initial_metadata_value".parse().unwrap();
    let key2 = "x-grpc-test-echo-trailing-bin";
    let value2 = MetadataValue::from_bytes(&[0xab, 0xab, 0xab]);

    let req = SimpleRequest {
        response_type: PayloadType::Compressable as i32,
        response_size: LARGE_RSP_SIZE,
        payload: Some(crate::client_payload(LARGE_REQ_SIZE)),
        ..Default::default()
    };
    let mut req_unary = Request::new(req);
    req_unary.metadata_mut().insert(key1, value1.clone());
    req_unary.metadata_mut().insert_bin(key2, value2.clone());

    let stream = tokio_stream::once(make_ping_pong_request(0));
    let mut req_stream = Request::new(stream);
    req_stream.metadata_mut().insert(key1, value1.clone());
    req_stream.metadata_mut().insert_bin(key2, value2.clone());

    let response = client
        .unary_call(req_unary)
        .await
        .expect("call should pass.");

    assertions.push(test_assert!(
        "metadata string must match in unary",
        response.metadata().get(key1) == Some(&value1),
        format!("result={:?}", response.metadata().get(key1))
    ));
    assertions.push(test_assert!(
        "metadata bin must match in unary",
        response.metadata().get_bin(key2) == Some(&value2),
        format!("result={:?}", response.metadata().get_bin(key1))
    ));

    let response = client
        .full_duplex_call(req_stream)
        .await
        .expect("call should pass.");

    assertions.push(test_assert!(
        "metadata string must match in unary",
        response.metadata().get(key1) == Some(&value1),
        format!("result={:?}", response.metadata().get(key1))
    ));

    let mut stream = response.into_inner();

    let trailers = stream.trailers().await.unwrap().unwrap();

    assertions.push(test_assert!(
        "metadata bin must match in unary",
        trailers.get_bin(key2) == Some(&value2),
        format!("result={:?}", trailers.get_bin(key1))
    ));
}

fn make_ping_pong_request(idx: usize) -> StreamingOutputCallRequest {
    let req_len = REQUEST_LENGTHS[idx];
    let resp_len = RESPONSE_LENGTHS[idx];
    StreamingOutputCallRequest {
        response_parameters: vec![ResponseParameters::with_size(resp_len)],
        payload: Some(crate::client_payload(req_len as usize)),
        ..Default::default()
    }
}
