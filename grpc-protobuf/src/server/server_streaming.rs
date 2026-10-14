/*
 *
 * Copyright 2026 gRPC authors.
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

use grpc::async_trait;
use grpc::server::CallOptions;
use grpc::server::DynHandle;
use grpc::server::DynRecvStream;
use grpc::server::DynSendStream;
use grpc::server::RequestHeaders;
use grpc::server::Trailers;
use protobuf::AsView;
use protobuf::Message;
use protobuf::Proxied;

use crate::ProtoRecvMessage;
use crate::SendFuture;
use crate::ServerStatus;
use crate::ServerStatusError;
use crate::StatusCodeError;
use crate::server::GrpcStreamingResponse;
use crate::trailers_conv::trailers_from_status;

/// A server-streaming RPC method handler on the server.
///
/// Implementations receive a single request message and send a stream of
/// response messages to the client.
#[trait_variant::make(Send)]
pub trait ServerStreamingMethod: Sync + 'static {
    /// The protobuf request message type.
    type Request: Message;
    /// The protobuf response message type.
    type Response: Message;

    /// Handles a server-streaming RPC call.
    ///
    /// Receives a view of the incoming `request` message and uses `responses`
    /// to stream response messages back to the client, returning a
    /// [`ServerStatus`] when the handler has completed.
    async fn call(
        &self,
        request: <Self::Request as Proxied>::View<'_>,
        responses: GrpcStreamingResponse<'_, Self::Response>,
    ) -> ServerStatus;
}

/// An adapter that wraps a [`ServerStreamingMethod`] to handle incoming
/// server-streaming RPCs.
pub struct ServerStreamingAdapter<M> {
    method: M,
}

impl<M> ServerStreamingAdapter<M> {
    /// Creates a new [`ServerStreamingAdapter`] wrapping the given `method`.
    pub fn new(method: M) -> Self {
        Self { method }
    }
}

#[async_trait]
impl<M> DynHandle for ServerStreamingAdapter<M>
where
    M: ServerStreamingMethod,
{
    async fn dyn_handle(
        &self,
        _headers: RequestHeaders,
        _options: CallOptions,
        tx: &mut dyn DynSendStream,
        mut rx: Box<dyn DynRecvStream>,
    ) -> Trailers {
        let mut req = <M::Request as Default>::default();

        match rx.dyn_next(&mut ProtoRecvMessage::from_mut(&mut req)).await {
            None => {
                return trailers_from_status(Err(ServerStatusError::new(
                    StatusCodeError::Internal,
                    "unary stream received zero messages",
                )));
            }
            Some(Err(_)) => {
                return trailers_from_status(Err(ServerStatusError::new(
                    StatusCodeError::Internal,
                    "stream failure",
                )));
            }
            Some(Ok(_)) => {}
        }

        match rx.dyn_next(&mut ProtoRecvMessage::from_mut(&mut req)).await {
            None => {}
            Some(Err(_)) => {
                return trailers_from_status(Err(ServerStatusError::new(
                    StatusCodeError::Internal,
                    "stream failure",
                )));
            }
            Some(Ok(_)) => {
                return trailers_from_status(Err(ServerStatusError::new(
                    StatusCodeError::Internal,
                    "unary stream received multiple messages",
                )));
            }
        }

        let responses = GrpcStreamingResponse::new(tx);
        let status = self.method.call(req.as_view(), responses).make_send().await;
        trailers_from_status(status)
    }
}
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use grpc::core::Address;
    use grpc::core::ConnectionInfo;
    use grpc::core::RecvMessage;
    use grpc::core::StreamEnded;
    use grpc::core::StreamError;
    use grpc::credentials::SecurityInfo;
    use grpc::server::RecvStream;
    use grpc::server::ResponseStreamItem;
    use grpc::server::SendOptions;
    use grpc::server::SendStream;
    use protobuf_well_known_types::Any;

    use super::*;

    struct TestServerStreamingMethod {
        called: Arc<AtomicBool>,
    }

    impl ServerStreamingMethod for TestServerStreamingMethod {
        type Request = Any;
        type Response = Any;

        async fn call(
            &self,
            _request: <Self::Request as Proxied>::View<'_>,
            _responses: GrpcStreamingResponse<'_, Self::Response>,
        ) -> ServerStatus {
            self.called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct MockSendStream;

    impl SendStream for MockSendStream {
        async fn send<'a>(
            &mut self,
            _item: ResponseStreamItem<'a>,
            _options: SendOptions,
        ) -> Result<(), StreamEnded> {
            Ok(())
        }
    }

    struct MockRecvStream {
        items: VecDeque<Option<Result<(), StreamError>>>,
    }

    impl MockRecvStream {
        fn new(items: impl IntoIterator<Item = Option<Result<(), StreamError>>>) -> Self {
            Self {
                items: items.into_iter().collect(),
            }
        }
    }

    impl RecvStream for MockRecvStream {
        async fn next(&mut self, _msg: &mut dyn RecvMessage) -> Option<Result<(), StreamError>> {
            self.items.pop_front().unwrap_or(None)
        }
    }

    async fn run_test(
        called: &Arc<AtomicBool>,
        items: impl IntoIterator<Item = Option<Result<(), StreamError>>>,
    ) -> Trailers {
        let adapter = ServerStreamingAdapter::new(TestServerStreamingMethod {
            called: called.clone(),
        });

        let connection_info = ConnectionInfo::new(
            Address::default(),
            Address::default(),
            SecurityInfo::new(""),
        );
        let headers = RequestHeaders::new("/test.TestService/TestMethod", connection_info);

        let mut tx = MockSendStream;
        let rx: Box<dyn DynRecvStream> = Box::new(MockRecvStream::new(items));

        adapter
            .dyn_handle(headers, CallOptions::default(), &mut tx, rx)
            .await
    }

    #[tokio::test]
    async fn test_server_streaming_zero_messages_returns_error() {
        let called = Arc::new(AtomicBool::new(false));
        let trailers = run_test(&called, []).await;

        let status = trailers
            .status()
            .as_ref()
            .expect_err("expected error status in trailers when client sends 0 messages");
        assert_eq!(status.code(), grpc::StatusCodeError::Internal);
        assert_eq!(status.message(), "unary stream received zero messages");
        assert!(
            !called.load(Ordering::SeqCst),
            "method should not be called when request stream sends 0 messages"
        );
    }

    #[tokio::test]
    async fn test_server_streaming_multiple_messages_returns_error() {
        let called = Arc::new(AtomicBool::new(false));
        let trailers = run_test(&called, [Some(Ok(())), Some(Ok(()))]).await;

        let status = trailers
            .status()
            .as_ref()
            .expect_err("expected error status in trailers when client sends multiple messages");
        assert_eq!(status.code(), grpc::StatusCodeError::Internal);
        assert_eq!(status.message(), "unary stream received multiple messages");
        assert!(
            !called.load(Ordering::SeqCst),
            "method should not be called when request stream sends multiple messages"
        );
    }

    #[tokio::test]
    async fn test_server_streaming_stream_failure_on_first_message_returns_error() {
        let called = Arc::new(AtomicBool::new(false));
        let trailers = run_test(&called, [Some(Err(StreamError))]).await;

        let status = trailers
            .status()
            .as_ref()
            .expect_err("expected error status in trailers when stream fails on first message");
        assert_eq!(status.code(), grpc::StatusCodeError::Internal);
        assert_eq!(status.message(), "stream failure");
        assert!(
            !called.load(Ordering::SeqCst),
            "method should not be called when request stream fails on first message"
        );
    }

    #[tokio::test]
    async fn test_server_streaming_stream_failure_on_second_message_returns_error() {
        let called = Arc::new(AtomicBool::new(false));
        let trailers = run_test(&called, [Some(Ok(())), Some(Err(StreamError))]).await;

        let status = trailers
            .status()
            .as_ref()
            .expect_err("expected error status in trailers when stream fails after one message");
        assert_eq!(status.code(), grpc::StatusCodeError::Internal);
        assert_eq!(status.message(), "stream failure");
        assert!(
            !called.load(Ordering::SeqCst),
            "method should not be called when request stream fails after one message"
        );
    }

    #[tokio::test]
    async fn test_server_streaming_single_message_success() {
        let called = Arc::new(AtomicBool::new(false));
        let trailers = run_test(&called, [Some(Ok(())), None]).await;

        assert!(trailers.status().is_ok());
        assert!(
            called.load(Ordering::SeqCst),
            "method should be called when request stream has exactly one message"
        );
    }
}
