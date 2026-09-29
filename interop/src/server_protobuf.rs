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

use std::time::Duration;

use grpc::StatusError;
use grpc::server::CallOptions;
use grpc::server::Handle;
use grpc::server::RecvStream;
use grpc::server::RequestHeaders;
use grpc::server::ResponseHeaders;
use grpc::server::ResponseStreamItem;
use grpc::server::SendOptions;
use grpc::server::SendStream;
use grpc::server::Trailers;
use grpc::server::interceptor::Intercept;
use grpc_protobuf::ServerStatus;
use grpc_protobuf::ServerStatusError;
use grpc_protobuf::StatusCodeError;
use grpc_protobuf::server::GrpcStreamingRequest;
use grpc_protobuf::server::GrpcStreamingResponse;
use protobuf::proto;

use crate::grpc_pb::test_service_server::TestService;
pub use crate::grpc_pb::test_service_server::TestServiceServer;
use crate::grpc_pb::unimplemented_service_server::UnimplementedService;
pub use crate::grpc_pb::unimplemented_service_server::UnimplementedServiceServer;
use crate::grpc_pb::*;
use crate::grpc_utils;

#[derive(Default, Clone)]
pub struct InteropTestService {}

#[grpc::async_trait]
impl TestService for InteropTestService {
    async fn empty_call(&self, _request: EmptyView<'_>, _response: EmptyMut<'_>) -> ServerStatus {
        Ok(())
    }

    async fn unary_call(
        &self,
        request: SimpleRequestView<'_>,
        mut response: SimpleResponseMut<'_>,
    ) -> ServerStatus {
        let code = request.response_status().code();
        if code != 0 {
            let status = ServerStatusError::new(
                StatusCodeError::from(code),
                request.response_status().message().to_string(),
            );
            return Err(status);
        }

        let res_size = if request.response_size() >= 0 {
            request.response_size() as usize
        } else {
            let status = ServerStatusError::new(
                StatusCodeError::InvalidArgument,
                "response_size cannot be negative",
            );
            return Err(status);
        };

        let payload = grpc_utils::server_payload(res_size);
        response.set_payload(payload);

        Ok(())
    }

    async fn streaming_output_call(
        &self,
        request: StreamingOutputCallRequestView<'_>,
        mut responses: GrpcStreamingResponse<'_, StreamingOutputCallResponse>,
    ) -> ServerStatus {
        for param in request.response_parameters() {
            tokio::time::sleep(Duration::from_micros(param.interval_us() as u64)).await;

            let payload = grpc_utils::server_payload(param.size() as usize);
            let response = proto!(StreamingOutputCallResponse { payload });
            if responses.send(&response).await.is_err() {
                break;
            }
        }

        Ok(())
    }

    async fn streaming_input_call(
        &self,
        mut request: GrpcStreamingRequest<StreamingInputCallRequest>,
        mut response: StreamingInputCallResponseMut<'_>,
    ) -> ServerStatus {
        let mut aggregated_payload_size = 0;
        loop {
            match request.recv().await {
                Some(Ok(msg)) => {
                    aggregated_payload_size += msg.payload().body().len() as i32;
                }
                Some(Err(_)) => {
                    return Err(ServerStatusError::new(
                        StatusCodeError::Internal,
                        "stream failure",
                    ));
                }
                None => break,
            }
        }

        response.set_aggregated_payload_size(aggregated_payload_size);

        Ok(())
    }

    async fn full_duplex_call(
        &self,
        mut request: GrpcStreamingRequest<StreamingOutputCallRequest>,
        mut responses: GrpcStreamingResponse<'_, StreamingOutputCallResponse>,
    ) -> ServerStatus {
        loop {
            let msg = match request.recv().await {
                Some(Ok(msg)) => msg,
                Some(Err(_)) => {
                    return Err(ServerStatusError::new(
                        StatusCodeError::Internal,
                        "stream failure",
                    ));
                }
                None => break,
            };

            let code = msg.response_status().code();
            if code != 0 {
                let status = ServerStatusError::new(
                    StatusCodeError::from(code),
                    msg.response_status().message().to_string(),
                );
                return Err(status);
            }

            for param in msg.response_parameters() {
                tokio::time::sleep(Duration::from_micros(param.interval_us() as u64)).await;

                let payload = grpc_utils::server_payload(param.size() as usize);
                let response = proto!(StreamingOutputCallResponse { payload: payload });
                if responses.send(&response).await.is_err() {
                    return Err(ServerStatusError::new(
                        StatusCodeError::Internal,
                        "stream failure",
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Default)]
pub struct UnimplementedInteropService {}

#[grpc::async_trait]
impl UnimplementedService for UnimplementedInteropService {}

#[derive(Debug, Clone)]
pub struct EchoHeaders {
    _priv: (),
}

impl EchoHeaders {
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl Default for EchoHeaders {
    fn default() -> Self {
        Self::new()
    }
}

impl Intercept for EchoHeaders {
    async fn intercept(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
        next: &impl Handle,
    ) -> Trailers {
        let echo_header = headers.metadata().get("x-grpc-test-echo-initial").cloned();
        let echo_trailer = headers
            .metadata()
            .get_bin("x-grpc-test-echo-trailing-bin")
            .cloned();

        if let Some(echo_header) = echo_header {
            let mut response_headers = ResponseHeaders::new();
            response_headers
                .metadata_mut()
                .insert("x-grpc-test-echo-initial", echo_header);
            if tx
                .send(
                    ResponseStreamItem::Headers(response_headers),
                    SendOptions::default(),
                )
                .await
                .is_err()
            {
                return Trailers::new(Err(StatusError::new(
                    grpc::StatusCodeError::Internal,
                    "failed to send headers",
                )));
            }
        }

        let mut trailers = next.handle(headers, options, tx, rx).await;

        if let Some(echo_trailer) = echo_trailer {
            trailers
                .metadata_mut()
                .insert_bin("x-grpc-test-echo-trailing-bin", echo_trailer);
        }

        trailers
    }
}
