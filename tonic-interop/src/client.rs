use crate::{pb::*, test_assert, TestAssertion};
use futures_util::{future, stream, SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::{net::TcpStream, sync::mpsc};
use tonic::{Request, Response};
use tower_h2::{add_origin::AddOrigin, Connection};

pub type Client = TestServiceClient<AddOrigin<Connection<tonic::BoxBody>>>;

tonic::client!(service = "grpc.testing.TestService", proto = "crate::pb");

const LARGE_REQ_SIZE: usize = 271828;
const LARGE_RSP_SIZE: i32 = 314159;
const REQUEST_LENGTHS: &'static [i32] = &[27182, 8, 1828, 45904];
const RESPONSE_LENGTHS: &'static [i32] = &[31415, 9, 2653, 58979];
// const TEST_STATUS_MESSAGE: &'static str = "test status message";
// const SPECIAL_TEST_STATUS_MESSAGE: &'static str =
//     "\t\ntest with whitespace\r\nand Unicode BMP ☺ and non-BMP 😈\t\n";

pub async fn create(addr: SocketAddr) -> Result<Client, Box<dyn std::error::Error>> {
    let io = TcpStream::connect(&addr).await?;

    let origin = http::Uri::from_shared(format!("http://{}", addr).into()).unwrap();

    let svc = Connection::handshake(io).await?;
    let svc = AddOrigin::new(svc, origin);

    Ok(TestServiceClient::new(svc))
}

pub async fn empty_unary(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
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

pub async fn large_unary(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
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

pub async fn client_streaming(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
    let requests = REQUEST_LENGTHS
        .iter()
        .map(|len| StreamingInputCallRequest {
            payload: Some(crate::client_payload(*len as usize)),
            ..Default::default()
        })
        .map(|v| Ok(v));

    let stream = stream::iter(requests);

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

pub async fn server_streaming(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
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
            .filter_map(|m| future::ready(m.ok()))
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

pub async fn ping_pong(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
    fn make_ping_pong_request(idx: usize) -> StreamingOutputCallRequest {
        let req_len = REQUEST_LENGTHS[idx];
        let resp_len = RESPONSE_LENGTHS[idx];
        StreamingOutputCallRequest {
            response_parameters: vec![ResponseParameters::with_size(resp_len)],
            payload: Some(crate::client_payload(req_len as usize)),
            ..Default::default()
        }
    }

    let (mut tx, rx) = mpsc::unbounded_channel();
    tx.try_send(make_ping_pong_request(0)).unwrap();

    let result = client
        .full_duplex_call(Request::new(rx.map(|s| Ok(s))))
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
                    // TODO: what to do with this result?
                    responses.push(result.unwrap());
                    if responses.len() == REQUEST_LENGTHS.len() {
                        drop(tx);
                        break;
                    } else {
                        tx.send(make_ping_pong_request(responses.len()))
                            .await
                            .unwrap();
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

pub async fn empty_stream(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
    let stream = stream::iter(Vec::new());
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
            responses.len() == 0,
            format!("responses.len()={:?}", responses.len())
        ));
    }
}
