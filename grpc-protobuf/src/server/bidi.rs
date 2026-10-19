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
use protobuf::Message;

use crate::ServerStatus;
use crate::server::GrpcStreamingRequest;
use crate::server::GrpcStreamingResponse;
use crate::trailers_conv::trailers_from_status;

/// A bidirectional-streaming RPC method handler on the server.
///
/// Implementations receive a stream of request messages and send a stream of
/// response messages to the client.
#[trait_variant::make(Send)]
pub trait BidiStreamingMethod: Sync + 'static {
    /// The protobuf request message type.
    type Request: Message;
    /// The protobuf response message type.
    type Response: Message;

    /// Handles a bidirectional-streaming RPC call.
    ///
    /// Receives incoming `requests` from the client and uses `responses` to
    /// stream response messages back to the client, returning a [`ServerStatus`]
    /// when the handler has completed.
    async fn call(
        &self,
        requests: GrpcStreamingRequest<Self::Request>,
        responses: GrpcStreamingResponse<'_, Self::Response>,
    ) -> ServerStatus;
}

/// An adapter that wraps a [`BidiStreamingMethod`] to handle incoming
/// bidirectional-streaming RPCs.
pub struct BidiStreamingAdapter<M> {
    method: M,
}

impl<M> BidiStreamingAdapter<M> {
    /// Creates a new [`BidiStreamingAdapter`] wrapping the given `method`.
    pub fn new(method: M) -> Self {
        Self { method }
    }
}

#[async_trait]
impl<M> DynHandle for BidiStreamingAdapter<M>
where
    M: BidiStreamingMethod,
{
    async fn dyn_handle(
        &self,
        _headers: RequestHeaders,
        _options: CallOptions,
        tx: &mut dyn DynSendStream,
        rx: Box<dyn DynRecvStream>,
    ) -> Trailers {
        // The request stream owns `rx`; the response sink borrows `tx`. They
        // are independent, so a handler can freely interleave receives and
        // sends.
        let requests = GrpcStreamingRequest::new(rx);
        let responses = GrpcStreamingResponse::new(tx);
        let status = self.method.call(requests, responses).await;
        trailers_from_status(status)
    }
}

#[cfg(test)]
mod tests {
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

    struct TestBidiStreamingMethod {
        called: Arc<AtomicBool>,
    }

    impl BidiStreamingMethod for TestBidiStreamingMethod {
        type Request = Any;
        type Response = Any;

        async fn call(
            &self,
            mut requests: GrpcStreamingRequest<Self::Request>,
            _responses: GrpcStreamingResponse<'_, Self::Response>,
        ) -> ServerStatus {
            self.called.store(true, Ordering::SeqCst);
            assert!(requests.recv().await.is_none());
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

    struct EmptyRecvStream;

    impl RecvStream for EmptyRecvStream {
        async fn next(&mut self, _msg: &mut dyn RecvMessage) -> Option<Result<(), StreamError>> {
            None
        }
    }

    #[tokio::test]
    async fn test_bidi_streaming_empty_stream_success() {
        let called = Arc::new(AtomicBool::new(false));
        let adapter = BidiStreamingAdapter::new(TestBidiStreamingMethod {
            called: called.clone(),
        });

        let connection_info = ConnectionInfo::new(
            Address::default(),
            Address::default(),
            SecurityInfo::new(""),
        );
        let headers = RequestHeaders::new("/test.TestService/TestMethod", connection_info);

        let mut tx = MockSendStream;
        let rx: Box<dyn DynRecvStream> = Box::new(EmptyRecvStream);

        let trailers = adapter
            .dyn_handle(headers, CallOptions::default(), &mut tx, rx)
            .await;

        assert!(trailers.status().is_ok());
        assert!(called.load(Ordering::SeqCst));
    }
}
