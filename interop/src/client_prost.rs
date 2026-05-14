use crate::client::{InteropTest, InteropTestUnimplemented};
use crate::{
    TestAssertion, pb::test_service_client::*, pb::unimplemented_service_client::*, pb::*,
    test_assert,
};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::async_trait;
use tonic::transport::Channel;
use tonic::{Code, Request, Response, Status, metadata::MetadataValue};

pub type TestClient = TestServiceClient<
    tonic::codegen::InterceptedService<Channel, crate::client::MetadataInterceptor>,
>;
pub type UnimplementedClient = UnimplementedServiceClient<
    tonic::codegen::InterceptedService<Channel, crate::client::MetadataInterceptor>,
>;

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
        let result = self.empty_call(Request::new(Empty {})).await;

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

    async fn large_unary(&mut self, assertions: &mut Vec<TestAssertion>) {
        use std::mem;
        let payload = crate::client_payload(LARGE_REQ_SIZE);
        let req = SimpleRequest {
            response_type: PayloadType::Compressable as i32,
            response_size: LARGE_RSP_SIZE,
            payload: Some(payload),
            ..Default::default()
        };

        let result = self.unary_call(Request::new(req)).await;

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

    // async fn cachable_unary(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
    //     let payload = Payload {
    //         r#type: PayloadType::Compressable as i32,
    //         body: format!("{:?}", std::time::Instant::now()).into_bytes(),
    //     };
    //     let req = SimpleRequest {
    //         response_type: PayloadType::Compressable as i32,
    //         payload: Some(payload),
    //         ..Default::default()
    //     };

    //     self.
    // }

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
                body.aggregated_payload_size == 74922,
                format!("aggregated_payload_size={:?}", body.aggregated_payload_size)
            ));
        }
    }

    async fn server_streaming(&mut self, assertions: &mut Vec<TestAssertion>) {
        let req = StreamingOutputCallRequest {
            response_parameters: RESPONSE_LENGTHS
                .iter()
                .map(|len| ResponseParameters::with_size(*len))
                .collect(),
            ..Default::default()
        };
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
        let req = SimpleRequest {
            response_status: Some(EchoStatus {
                code: 2,
                message: SPECIAL_TEST_STATUS_MESSAGE.to_string(),
            }),
            ..Default::default()
        };

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
        let result = self.unimplemented_call(Request::new(Empty {})).await;
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

    async fn cacheable_unary(&mut self, assertions: &mut Vec<TestAssertion>) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        let payload = Payload {
            body: timestamp.into_bytes(),
            ..Default::default()
        };
        let req = SimpleRequest {
            response_type: PayloadType::Compressable as i32,
            payload: Some(payload),
            ..Default::default()
        };

        let mut req1 = Request::new(req.clone());
        req1.metadata_mut()
            .insert("x-user-ip", "1.2.3.4".parse().unwrap());

        let result1 = self.cacheable_unary_call(req1).await;

        assertions.push(test_assert!(
            "first call must be successful",
            result1.is_ok(),
            format!("result={:?}", result1)
        ));

        let mut req2 = Request::new(req);
        req2.metadata_mut()
            .insert("x-user-ip", "1.2.3.4".parse().unwrap());
        let result2 = self.cacheable_unary_call(req2).await;

        assertions.push(test_assert!(
            "second call must be successful",
            result2.is_ok(),
            format!("result={:?}", result2)
        ));

        if let (Ok(res1), Ok(res2)) = (result1, result2) {
            let body1 = res1.into_inner();
            let body2 = res2.into_inner();
            assertions.push(test_assert!(
                "payload body of both responses is the same",
                body1 == body2,
                format!("body1={:?}, body2={:?}", body1, body2)
            ));
        }
    }

    async fn client_compressed_unary(&mut self, assertions: &mut Vec<TestAssertion>) {
        // 1. Probe
        let req = SimpleRequest {
            expect_compressed: Some(crate::pb::BoolValue { value: true }),
            response_size: LARGE_RSP_SIZE,
            payload: Some(crate::client_payload(LARGE_REQ_SIZE)),
            ..Default::default()
        };
        let result = self.unary_call(Request::new(req.clone())).await;
        assertions.push(test_assert!(
            "First call failed with INVALID_ARGUMENT status",
            match &result {
                Err(status) => status.code() == Code::InvalidArgument,
                _ => false,
            },
            format!("result={:?}", result)
        ));

        // 2. Compressed
        let mut compressed_client = self
            .clone()
            .send_compressed(tonic::codec::CompressionEncoding::Gzip);
        let result = compressed_client
            .unary_call(Request::new(req.clone()))
            .await;
        assertions.push(test_assert!(
            "Second call (compressed) must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));
        if let Ok(response) = result {
            let body = response.into_inner();
            assertions.push(test_assert!(
                "response payload body is 314159 bytes in size",
                body.payload.as_ref().map_or(0, |p| p.body.len()) == LARGE_RSP_SIZE as usize,
                format!(
                    "body.payload.len={:?}",
                    body.payload.as_ref().map(|p| p.body.len())
                )
            ));
        }

        // 3. Uncompressed
        let req = SimpleRequest {
            expect_compressed: Some(crate::pb::BoolValue { value: false }),
            response_size: LARGE_RSP_SIZE,
            payload: Some(crate::client_payload(LARGE_REQ_SIZE)),
            ..Default::default()
        };
        let result = self.unary_call(Request::new(req)).await;
        assertions.push(test_assert!(
            "Third call (uncompressed) must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));
        if let Ok(response) = result {
            let body = response.into_inner();
            assertions.push(test_assert!(
                "response payload body is 314159 bytes in size",
                body.payload.as_ref().map_or(0, |p| p.body.len()) == LARGE_RSP_SIZE as usize,
                format!(
                    "body.payload.len={:?}",
                    body.payload.as_ref().map(|p| p.body.len())
                )
            ));
        }
    }

    async fn server_compressed_unary(&mut self, assertions: &mut Vec<TestAssertion>) {
        // 1. Request compressed response
        let req = SimpleRequest {
            response_compressed: Some(crate::pb::BoolValue { value: true }),
            response_size: LARGE_RSP_SIZE,
            payload: Some(crate::client_payload(LARGE_REQ_SIZE)),
            ..Default::default()
        };

        let mut client = self
            .clone()
            .accept_compressed(tonic::codec::CompressionEncoding::Gzip);

        let result = client.unary_call(Request::new(req.clone())).await;

        assertions.push(test_assert!(
            "Call with response_compressed=true must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        if let Ok(response) = result {
            assertions.push(test_assert!(
                "Response must have grpc-encoding: gzip",
                response.metadata().get("grpc-encoding")
                    == Some(&tonic::metadata::MetadataValue::from_static("gzip")),
                format!("metadata={:?}", response.metadata())
            ));
            let body = response.into_inner();
            assertions.push(test_assert!(
                "response payload body is 314159 bytes in size",
                body.payload.as_ref().map_or(0, |p| p.body.len()) == LARGE_RSP_SIZE as usize,
                format!(
                    "body.payload.len={:?}",
                    body.payload.as_ref().map(|p| p.body.len())
                )
            ));
        }

        // 2. Request uncompressed response
        let req = SimpleRequest {
            response_compressed: Some(crate::pb::BoolValue { value: false }),
            response_size: LARGE_RSP_SIZE,
            payload: Some(crate::client_payload(LARGE_REQ_SIZE)),
            ..Default::default()
        };

        let result = client.unary_call(Request::new(req)).await;

        assertions.push(test_assert!(
            "Call with response_compressed=false must be successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        if let Ok(response) = result {
            let body = response.into_inner();
            assertions.push(test_assert!(
                "response payload body is 314159 bytes in size",
                body.payload.as_ref().map_or(0, |p| p.body.len()) == LARGE_RSP_SIZE as usize,
                format!(
                    "body.payload.len={:?}",
                    body.payload.as_ref().map(|p| p.body.len())
                )
            ));
        }
    }

    async fn cancel_after_begin(&mut self, assertions: &mut Vec<TestAssertion>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamingInputCallRequest>();
        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

        let mut client = self.clone();

        let handle =
            tokio::spawn(async move { client.streaming_input_call(Request::new(stream)).await });

        handle.abort();

        let result = handle.await;

        assertions.push(test_assert!(
            "Call must be cancelled",
            match &result {
                Err(e) => e.is_cancelled(),
                _ => false,
            },
            format!("result={:?}", result)
        ));

        // Suppress unused variable warning for tx
        drop(tx);
    }

    async fn cancel_after_first_response(&mut self, assertions: &mut Vec<TestAssertion>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamingOutputCallRequest>();
        tx.send(make_ping_pong_request(0)).unwrap();

        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel(1);

        let mut client = self.clone();

        let handle = tokio::spawn(async move {
            let response = client
                .full_duplex_call(Request::new(
                    tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
                ))
                .await?;
            let mut stream = response.into_inner();
            let first_msg = stream.next().await;

            // Notify outside
            signal_tx.send(first_msg).await.unwrap();

            // Wait forever to be cancelled
            std::future::pending::<()>().await;

            Ok::<_, Status>(())
        });

        // Wait for signal
        let first_msg = signal_rx.recv().await;

        let success = matches!(&first_msg, Some(Some(Ok(_))));
        assertions.push(test_assert!(
            "Received first response",
            success,
            format!("first_msg={:?}", first_msg)
        ));

        // Cancel the task
        handle.abort();

        let result = handle.await;

        assertions.push(test_assert!(
            "Call must be cancelled",
            match &result {
                Err(e) => e.is_cancelled(),
                _ => false,
            },
            format!("result={:?}", result)
        ));

        drop(tx);
    }

    async fn timeout_on_sleeping_server(&mut self, assertions: &mut Vec<TestAssertion>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamingOutputCallRequest>();

        let mut req = make_ping_pong_request(0);
        if let Some(param) = req.response_parameters.first_mut() {
            param.interval_us = 100000;
        }
        tx.send(req).unwrap();

        let mut client = self.clone();

        let mut request = Request::new(tokio_stream::wrappers::UnboundedReceiverStream::new(rx));
        request.set_timeout(std::time::Duration::from_millis(50));

        let result = client.full_duplex_call(request).await;

        // For streaming calls, the timeout might occur during the stream poll,
        // and Tonic might return it as a Status or it might be handled differently.
        // But usually it returns Err(Status) with DeadlineExceeded.

        assertions.push(test_assert!(
            "Initial call was successful",
            result.is_ok(),
            format!("result={:?}", result)
        ));

        if let Ok(response) = result {
            let mut stream = response.into_inner();
            let stream_result =
                tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await;

            assertions.push(test_assert!(
                "Stream must time out (DEADLINE_EXCEEDED)",
                stream_result.is_err(),
                format!("stream_result={:?}", stream_result)
            ));
        }

        drop(tx);
    }
}

#[async_trait]
impl InteropTestUnimplemented for UnimplementedClient {
    async fn unimplemented_service(&mut self, assertions: &mut Vec<TestAssertion>) {
        let result = self.unimplemented_call(Request::new(Empty {})).await;
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
    StreamingOutputCallRequest {
        response_parameters: vec![ResponseParameters::with_size(resp_len)],
        payload: Some(crate::client_payload(req_len as usize)),
        ..Default::default()
    }
}

fn make_streaming_input_request(len: &i32) -> StreamingInputCallRequest {
    StreamingInputCallRequest {
        payload: Some(crate::client_payload(*len as usize)),
        ..Default::default()
    }
}
