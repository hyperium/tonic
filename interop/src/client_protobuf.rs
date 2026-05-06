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

use grpc::StatusCodeError;
use grpc::StatusOr;
use grpc::client::Channel;
use grpc::client::metadata_utils::AttachHeadersInterceptor;
use grpc::client::metadata_utils::CaptureHeadersInterceptor;
use grpc::client::metadata_utils::CaptureTrailersInterceptor;
use grpc_protobuf::CallBuilder;
use protobuf::message_eq;
use protobuf::proto;
use tonic::async_trait;
use tonic::metadata::MetadataMap;
use tonic::metadata::MetadataValue;

use crate::TestAssertion;
use crate::client::InteropTest;
use crate::client::InteropTestUnimplemented;
use crate::grpc_pb::test_service_client::*;
use crate::grpc_pb::unimplemented_service_client::*;
use crate::grpc_pb::*;
use crate::test_assert;

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
        let result = self.empty_call(proto!(Empty {})).await;

        assertions.push(test_assert!(
            "call must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        let Ok(response) = result else {
            return;
        };

        assertions.push(test_assert!(
            "body must not be null",
            message_eq(&response, &proto!(Empty {})),
            format!("result={:?}", response)
        ));
    }

    async fn large_unary(&mut self, assertions: &mut Vec<TestAssertion>) {
        use std::mem;
        let payload = crate::grpc_utils::client_payload(LARGE_REQ_SIZE);
        let req = proto!(SimpleRequest {
            response_type: PayloadType::Compressable,
            response_size: LARGE_RSP_SIZE,
            payload: payload,
        });

        let mut result = SimpleResponse::new();
        let status = self
            .unary_call(req)
            .with_response_message(&mut result)
            .await;

        assertions.push(test_assert!(
            "call must be successful",
            status.is_ok(),
            format!("status={status:?}")
        ));

        let body = result.payload().body();
        let payload_len = body.len();

        assertions.push(test_assert!(
            "body must be 314159 bytes",
            payload_len == LARGE_RSP_SIZE as usize,
            format!("mem::size_of_val(&body)={:?}", mem::size_of_val(body))
        ));
    }

    async fn client_streaming(&mut self, assertions: &mut Vec<TestAssertion>) {
        let mut stream = self.streaming_input_call().await;

        for request in REQUEST_LENGTHS.iter().map(make_streaming_input_request) {
            let _ = stream.send(&request).await;
        }

        let result = stream.close_and_recv().await;

        assertions.push(test_assert!(
            "call must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        if let Ok(response) = result {
            assertions.push(test_assert!(
                "aggregated payload size must be 74922 bytes",
                response.aggregated_payload_size() == 74922,
                format!(
                    "aggregated_payload_size={:?}",
                    response.aggregated_payload_size()
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

        let mut rx = self.streaming_output_call(req).await;

        let mut responses = Vec::new();
        while let Some(response) = rx.recv().await {
            responses.push(response);
        }

        let status = rx.status().await;
        assertions.push(test_assert!(
            "call must be successful",
            status.is_ok(),
            format!("result={status:?}")
        ));

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

    async fn ping_pong(&mut self, assertions: &mut Vec<TestAssertion>) {
        let (mut tx, mut rx) = self.full_duplex_call().await;
        let _ = tx.send(make_ping_pong_request(0)).await;

        let mut responses = Vec::new();
        loop {
            match rx.recv().await {
                Some(message) => {
                    responses.push(message);
                    if responses.len() == RESPONSE_LENGTHS.len() {
                        drop(tx);
                        break;
                    } else {
                        let _ = tx.send(make_ping_pong_request(responses.len())).await;
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

        let status = rx.status().await;
        assertions.push(test_assert!(
            "call must be successful",
            status.is_ok(),
            format!("result={status:?}")
        ));
    }

    async fn empty_stream(&mut self, assertions: &mut Vec<TestAssertion>) {
        let (tx, mut rx) = self.full_duplex_call().await;
        drop(tx);

        let mut responses = Vec::new();
        while let Some(response) = rx.recv().await {
            responses.push(response);
        }
        assertions.push(test_assert!(
            "there should be no responses",
            responses.is_empty(),
            format!("responses.len()={:?}", responses.len())
        ));

        let status = rx.status().await;
        assertions.push(test_assert!(
            "call must be successful",
            status.is_ok(),
            format!("result={status:?}")
        ));
    }

    async fn status_code_and_message(&mut self, assertions: &mut Vec<TestAssertion>) {
        fn validate_response<T>(result: StatusOr<T>, assertions: &mut Vec<TestAssertion>)
        where
            T: std::fmt::Debug,
        {
            assertions.push(test_assert!(
                "call must fail with unknown status code",
                match &result {
                    Err(status_err) => status_err.code() == StatusCodeError::Unknown,
                    _ => false,
                },
                format!("result={:?}", result)
            ));

            assertions.push(test_assert!(
                "call must respsond with expected status message",
                match &result {
                    Err(status_err) => status_err.message() == TEST_STATUS_MESSAGE,
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

        let result = self.unary_call(simple_req).await;
        validate_response(result, assertions);

        let (mut tx, mut rx) = self.full_duplex_call().await;
        let _ = tx.send(duplex_req).await;
        drop(tx);
        let mut responses = Vec::new();
        while let Some(response) = rx.recv().await {
            responses.push(response);
        }
        let result = rx.status().await.map(|()| responses);
        validate_response(result, assertions);
    }

    async fn special_status_message(&mut self, assertions: &mut Vec<TestAssertion>) {
        let req = proto!(SimpleRequest {
            response_status: EchoStatus {
                code: 2,
                message: SPECIAL_TEST_STATUS_MESSAGE.to_string(),
            },
        });

        let result = self.unary_call(req).await;

        assertions.push(test_assert!(
            "call must fail with unknown status code",
            match &result {
                Err(status) => status.code() == StatusCodeError::Unknown,
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
        let result = self.unimplemented_call(Empty::default()).await;
        assertions.push(test_assert!(
            "call must fail with unimplemented status code",
            match &result {
                Err(status) => status.code() == StatusCodeError::Unimplemented,
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
        let mut md = MetadataMap::new();
        md.insert(key1, value1.clone());
        md.insert_bin(key2, value2.clone());

        // First perform the unary call test.

        let req = proto!(SimpleRequest {
            response_type: PayloadType::Compressable,
            response_size: LARGE_RSP_SIZE,
            payload: crate::grpc_utils::client_payload(LARGE_REQ_SIZE),
        });

        let attacher = AttachHeadersInterceptor::new(md.clone());
        let (hdr_int, hdr_rx) = CaptureHeadersInterceptor::new();
        let (trl_int, trl_rx) = CaptureTrailersInterceptor::new();

        self.unary_call(req)
            .with_interceptor(attacher)
            .with_once_interceptor(hdr_int)
            .with_once_interceptor(trl_int)
            .await
            .expect("call should pass.");

        let response_headers = hdr_rx.await.expect("headers should be received");
        let response_trailers = trl_rx.await.expect("trailers should be received");

        assertions.push(test_assert!(
            "metadata string must match in unary",
            response_headers.get(key1) == Some(&value1),
            format!("result={:?}", response_headers.get(key1))
        ));
        assertions.push(test_assert!(
            "metadata bin must match in unary",
            response_trailers.get_bin(key2) == Some(&value2),
            format!("result={:?}", response_trailers.get_bin(key1))
        ));

        // Now perform the streaming call test.

        let attacher = AttachHeadersInterceptor::new(md.clone());
        let (hdr_int, hdr_rx) = CaptureHeadersInterceptor::new();
        let (trl_int, trl_rx) = CaptureTrailersInterceptor::new();

        let (mut tx, rx) = self
            .full_duplex_call()
            .with_interceptor(attacher)
            .with_once_interceptor(hdr_int)
            .with_once_interceptor(trl_int)
            .await;
        _ = tx.send(make_ping_pong_request(0)).await;
        drop(tx);
        let status = rx.status().await;
        assert!(status.is_ok(), "call should pass: {:?}", status);

        let response_headers = hdr_rx.await.expect("headers should be received");
        let response_trailers = trl_rx.await.expect("trailers should be received");
        assertions.push(test_assert!(
            "metadata string must match in unary",
            response_headers.get(key1) == Some(&value1),
            format!("result={:?}", response_headers.get(key1))
        ));

        assertions.push(test_assert!(
            "metadata bin must match in unary",
            response_trailers.get_bin(key2) == Some(&value2),
            format!("result={:?}", response_trailers.get_bin(key1))
        ));
    }
}

#[async_trait]
impl InteropTestUnimplemented for UnimplementedClient {
    async fn unimplemented_service(&mut self, assertions: &mut Vec<TestAssertion>) {
        let result = self.unimplemented_call(Empty::default()).await;
        assertions.push(test_assert!(
            "call must fail with unimplemented status code",
            match &result {
                Err(status) => status.code() == StatusCodeError::Unimplemented,
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
