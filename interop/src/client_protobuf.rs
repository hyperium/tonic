/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use crate::client::{InteropTest, InteropTestUnimplemented};
use crate::{
    TestAssertion, grpc_pb::test_service_client::*, grpc_pb::unimplemented_service_client::*,
    grpc_pb::*, test_assert,
};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::async_trait;
use tonic::transport::Channel;
use tonic::{Code, Request, Response, Status, metadata::MetadataValue};
use tonic_protobuf::protobuf::__internal::MatcherEq;
use tonic_protobuf::protobuf::proto;

pub type TestClient = TestServiceClient<Channel>;
pub type UnimplementedClient = UnimplementedServiceClient<Channel>;

const LARGE_REQ_SIZE: usize = 271_828;
const LARGE_RSP_SIZE: i32 = 314_159;
const REQUEST_LENGTHS: &[i32] = &[27182, 8, 1828, 45904];
const RESPONSE_LENGTHS: &[i32] = &[31415, 9, 2653, 58979];
const TEST_STATUS_MESSAGE: &str = "test status message";
const SPECIAL_TEST_STATUS_MESSAGE: &str =
    "\t\ntest with whitespace\r\nand Unicode BMP ☺ and non-BMP 😈\t\n";

#[async_trait]
impl InteropTest for TestClient {
    async fn empty_unary(&mut self, assertions: &mut Vec<TestAssertion>) {
        let result = self.empty_call(Request::new(Empty::default())).await;

        assertions.push(test_assert!(
            "call must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        if let Ok(response) = result {
            let body = response.into_inner();
            assertions.push(test_assert!(
                "body must not be null",
                body.matches(&Empty::default()),
                format!("body={:?}", body)
            ));
        }
    }

    async fn large_unary(&mut self, assertions: &mut Vec<TestAssertion>) {
        use std::mem;
        let payload = crate::grpc_utils::client_payload(LARGE_REQ_SIZE);
        let req = proto!(SimpleRequest {
            response_type: PayloadType::Compressable,
            response_size: LARGE_RSP_SIZE,
            payload: payload,
        });

        let result = self.unary_call(Request::new(req)).await;

        assertions.push(test_assert!(
            "call must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        if let Ok(response) = result {
            let body = response.into_inner();
            let payload_len = body.payload().body().len();

            assertions.push(test_assert!(
                "body must be 314159 bytes",
                payload_len == LARGE_RSP_SIZE as usize,
                format!("mem::size_of_val(&body)={:?}", mem::size_of_val(&body))
            ));
        }
    }

    async fn client_streaming(&mut self, assertions: &mut Vec<TestAssertion>) {
        let requests: Vec<_> = REQUEST_LENGTHS
            .iter()
            .map(make_streaming_input_request)
            .collect();

        let stream = tokio_stream::iter(requests);

        let result = self.streaming_input_call(Request::new(stream)).await;

        assertions.push(test_assert!(
            "call must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        if let Ok(response) = result {
            let body = response.into_inner();

            assertions.push(test_assert!(
                "aggregated payload size must be 74922 bytes",
                body.aggregated_payload_size() == 74922,
                format!(
                    "aggregated_payload_size={:?}",
                    body.aggregated_payload_size()
                )
            ));
        }
    }

    async fn server_streaming(&mut self, assertions: &mut Vec<TestAssertion>) {
        let req = proto!(StreamingOutputCallRequest {
            response_parameters: RESPONSE_LENGTHS
                .iter()
                .map(|len| ResponseParameters::with_size(*len)),
        });
        let req = Request::new(req);

        let result = self.streaming_output_call(req).await;

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
            let actual_response_lengths = crate::grpc_utils::response_lengths(&responses);
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

    async fn ping_pong(&mut self, assertions: &mut Vec<TestAssertion>) {
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(make_ping_pong_request(0)).unwrap();

        let result = self
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

            let actual_response_lengths = crate::grpc_utils::response_lengths(&responses);
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

    async fn empty_stream(&mut self, assertions: &mut Vec<TestAssertion>) {
        let stream = tokio_stream::empty();
        let result = self.full_duplex_call(Request::new(stream)).await;

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

    async fn status_code_and_message(&mut self, assertions: &mut Vec<TestAssertion>) {
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

        let simple_req = proto!(SimpleRequest {
            response_status: EchoStatus {
                code: 2,
                message: TEST_STATUS_MESSAGE.to_string(),
            },
        });

        let duplex_req = proto!(StreamingOutputCallRequest {
            response_status: EchoStatus {
                code: 2,
                message: TEST_STATUS_MESSAGE.to_string(),
            },
        });

        let result = self.unary_call(Request::new(simple_req)).await;
        validate_response(result, assertions);

        let stream = tokio_stream::once(duplex_req);
        let result = match self.full_duplex_call(Request::new(stream)).await {
            Ok(response) => {
                let stream = response.into_inner();
                let responses = stream.collect::<Vec<_>>().await;
                Ok(responses)
            }
            Err(e) => Err(e),
        };

        validate_response(result, assertions);
    }

    async fn special_status_message(&mut self, assertions: &mut Vec<TestAssertion>) {
        let req = proto!(SimpleRequest {
            response_status: EchoStatus {
                code: 2,
                message: SPECIAL_TEST_STATUS_MESSAGE.to_string(),
            },
        });

        let result = self.unary_call(Request::new(req)).await;

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

    async fn unimplemented_method(&mut self, assertions: &mut Vec<TestAssertion>) {
        let result = self
            .unimplemented_call(Request::new(Empty::default()))
            .await;
        assertions.push(test_assert!(
            "call must fail with unimplemented status code",
            match &result {
                Err(status) => status.code() == Code::Unimplemented,
                _ => false,
            },
            format!("result={:?}", result)
        ));
    }

    async fn custom_metadata(&mut self, assertions: &mut Vec<TestAssertion>) {
        let key1 = "x-grpc-test-echo-initial";
        let value1: MetadataValue<_> = "test_initial_metadata_value".parse().unwrap();
        let key2 = "x-grpc-test-echo-trailing-bin";
        let value2 = MetadataValue::from_bytes(&[0xab, 0xab, 0xab]);

        let req = proto!(SimpleRequest {
            response_type: PayloadType::Compressable,
            response_size: LARGE_RSP_SIZE,
            payload: crate::grpc_utils::client_payload(LARGE_REQ_SIZE),
        });
        let mut req_unary = Request::new(req);
        req_unary.metadata_mut().insert(key1, value1.clone());
        req_unary.metadata_mut().insert_bin(key2, value2.clone());

        let stream = tokio_stream::once(make_ping_pong_request(0));
        let mut req_stream = Request::new(stream);
        req_stream.metadata_mut().insert(key1, value1.clone());
        req_stream.metadata_mut().insert_bin(key2, value2.clone());

        let response = self.unary_call(req_unary).await.expect("call should pass.");

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

        let response = self
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
}

#[async_trait]
impl InteropTestUnimplemented for UnimplementedClient {
    async fn unimplemented_service(&mut self, assertions: &mut Vec<TestAssertion>) {
        let result = self
            .unimplemented_call(Request::new(Empty::default()))
            .await;
        assertions.push(test_assert!(
            "call must fail with unimplemented status code",
            match &result {
                Err(status) => status.code() == Code::Unimplemented,
                _ => false,
            },
            format!("result={:?}", result)
        ));
    }
}

fn make_ping_pong_request(idx: usize) -> StreamingOutputCallRequest {
    let req_len = REQUEST_LENGTHS[idx];
    let resp_len = RESPONSE_LENGTHS[idx];
    proto!(StreamingOutputCallRequest {
        response_parameters: std::iter::once(ResponseParameters::with_size(resp_len)),
        payload: crate::grpc_utils::client_payload(req_len as usize),
    })
}

fn make_streaming_input_request(len: &i32) -> StreamingInputCallRequest {
    proto!(StreamingInputCallRequest {
        payload: crate::grpc_utils::client_payload(*len as usize),
    })
}
