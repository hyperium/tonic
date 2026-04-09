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

use crate::client::CallOptions;
use crate::core::{RecvMessage, RequestHeaders, ServerResponseStreamItem, Trailers};
use crate::server::interceptor::Intercept;
use crate::server::{Handle, RecvStream, SendOptions, SendStream};
use crate::{StatusCodeError, StatusError};
use tokio::sync::mpsc::channel;

struct ServerSendStreamValidator<S> {
    inner: S,
    state: SendStreamState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendStreamState {
    Init,
    HeadersSent,
    MessagesSent,
    Done,
}

impl<S: SendStream> ServerSendStreamValidator<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            state: SendStreamState::Init,
        }
    }
}

impl<S: SendStream> SendStream for ServerSendStreamValidator<S> {
    async fn send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()> {
        if self.state == SendStreamState::Done {
            // Protocol error: Attempted to send an item on a completed or failed stream.
            return Err(());
        }

        let next_state = match &item {
            ServerResponseStreamItem::Headers(_) => match self.state {
                SendStreamState::Init => SendStreamState::HeadersSent,
                _ => {
                    // Protocol error: Received multiple headers frames.
                    self.state = SendStreamState::Done;
                    return Err(());
                }
            },
            ServerResponseStreamItem::Message(_) => match self.state {
                SendStreamState::HeadersSent | SendStreamState::MessagesSent => {
                    SendStreamState::MessagesSent
                }
                _ => {
                    // Protocol error: Attempted to send a message before headers.
                    self.state = SendStreamState::Done;
                    return Err(());
                }
            },
        };

        let res = self.inner.send(item, options).await;
        match res {
            Ok(()) => self.state = next_state,
            Err(_) => {
                self.state = SendStreamState::Done;
            }
        }
        res
    }
}

struct ServerRecvStreamValidator<R> {
    inner: R,
    done: bool,
}

impl<R: RecvStream> ServerRecvStreamValidator<R> {
    fn new(inner: R) -> Self {
        Self { inner, done: false }
    }
}

impl<R: RecvStream> RecvStream for ServerRecvStreamValidator<R> {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
        if self.done {
            // Protocol error: Attempted to receive a message after reaching a terminal state (EOF or error).
            return Some(Err(()));
        }

        let res = self.inner.next(msg).await;
        match res {
            Some(Ok(())) => Some(Ok(())),
            None => {
                self.done = true;
                None
            }
            Some(Err(())) => {
                self.done = true;
                Some(Err(()))
            }
        }
    }
}

struct ChannelAwareSendStreamValidator<S> {
    inner: ServerSendStreamValidator<S>,
    error_tx: tokio::sync::mpsc::Sender<()>,
}

impl<S: SendStream> ChannelAwareSendStreamValidator<S> {
    fn new(inner: S, error_tx: tokio::sync::mpsc::Sender<()>) -> Self {
        Self {
            inner: ServerSendStreamValidator::new(inner),
            error_tx,
        }
    }

    fn report_error(&self) {
        let _ = self.error_tx.try_send(());
    }
}

impl<S: SendStream> SendStream for ChannelAwareSendStreamValidator<S> {
    async fn send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()> {
        let res = self.inner.send(item, options).await;
        if res.is_err() {
            self.report_error();
        }
        res
    }
}

struct ChannelAwareRecvStreamValidator<R> {
    inner: ServerRecvStreamValidator<R>,
    error_tx: tokio::sync::mpsc::Sender<()>,
}

impl<R: RecvStream> ChannelAwareRecvStreamValidator<R> {
    fn new(inner: R, error_tx: tokio::sync::mpsc::Sender<()>) -> Self {
        Self {
            inner: ServerRecvStreamValidator::new(inner),
            error_tx,
        }
    }

    fn report_error(&self) {
        let _ = self.error_tx.try_send(());
    }
}

impl<R: RecvStream> RecvStream for ChannelAwareRecvStreamValidator<R> {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
        let res = self.inner.next(msg).await;
        if let Some(Err(())) = res {
            self.report_error();
        }
        res
    }
}

pub struct StreamValidationInterceptor;

impl Intercept for StreamValidationInterceptor {
    async fn intercept(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
        tx: &mut impl SendStream,
        rx: impl RecvStream + 'static,
        next: &impl Handle,
    ) -> Trailers {
        let (error_tx, mut error_rx) = channel::<()>(1);
        let mut wrapped_tx = ChannelAwareSendStreamValidator::new(tx, error_tx.clone());
        let wrapped_rx = ChannelAwareRecvStreamValidator::new(rx, error_tx);

        tokio::select! {
            res = next.handle(headers, options, &mut wrapped_tx, wrapped_rx) => {
                if error_rx.try_recv().is_ok() {
                    Trailers::new(Err(StatusError::new(StatusCodeError::Internal, "Stream validation error")))
                } else {
                    res
                }
            }
            _ = error_rx.recv() => {
                Trailers::new(Err(StatusError::new(StatusCodeError::Internal, "Stream validation error")))
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::StatusCodeError;
    use crate::client::CallOptions;
    use crate::core::{
        RecvMessage, RequestHeaders, ResponseHeaders, SendMessage, ServerResponseStreamItem,
        Trailers,
    };
    use crate::server::SendOptions;
    use crate::server::interceptor::HandleExt;
    use bytes::{Buf, Bytes};

    impl SendMessage for () {
        fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
            Ok(Box::new(Bytes::new()))
        }
    }

    struct NopRecvMessage;
    impl RecvMessage for NopRecvMessage {
        fn decode(&mut self, _data: &mut dyn Buf) -> Result<(), String> {
            Ok(())
        }
    }

    struct MockSendStream;
    impl SendStream for MockSendStream {
        async fn send<'a>(
            &mut self,
            _item: ServerResponseStreamItem<'a>,
            _options: SendOptions,
        ) -> Result<(), ()> {
            Ok(())
        }
    }

    struct FailingMockSendStream;
    impl SendStream for FailingMockSendStream {
        async fn send<'a>(
            &mut self,
            _item: ServerResponseStreamItem<'a>,
            _options: SendOptions,
        ) -> Result<(), ()> {
            Err(())
        }
    }

    struct ConfigurableMockRecvStream {
        items: Vec<Option<Result<(), ()>>>,
        index: usize,
    }

    impl ConfigurableMockRecvStream {
        fn new(items: Vec<Option<Result<(), ()>>>) -> Self {
            Self { items, index: 0 }
        }
    }

    impl RecvStream for ConfigurableMockRecvStream {
        async fn next(&mut self, _msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
            if self.index < self.items.len() {
                let res = self.items[self.index];
                self.index += 1;
                res
            } else {
                None
            }
        }
    }

    #[tokio::test]
    async fn test_interceptor_successful_multi_message_streaming() {
        struct StreamingHandler;
        impl Handle for StreamingHandler {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                tx: &mut impl SendStream,
                mut rx: impl RecvStream + 'static,
            ) -> Trailers {
                let mut msg = NopRecvMessage;
                let mut recv_count = 0;
                while let Some(Ok(())) = rx.next(&mut msg).await {
                    recv_count += 1;
                }
                assert_eq!(recv_count, 3);

                tx.send(
                    ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                    SendOptions::default(),
                )
                .await
                .unwrap();

                tx.send(
                    ServerResponseStreamItem::Message(&()),
                    SendOptions::default(),
                )
                .await
                .unwrap();
                tx.send(
                    ServerResponseStreamItem::Message(&()),
                    SendOptions::default(),
                )
                .await
                .unwrap();

                Trailers::new(Ok(()))
            }
        }

        let chain = StreamingHandler.with_interceptor(StreamValidationInterceptor);
        let mut tx = MockSendStream;
        // Stream providing 3 valid messages followed by EOF
        let rx =
            ConfigurableMockRecvStream::new(vec![Some(Ok(())), Some(Ok(())), Some(Ok(())), None]);

        let trailers = chain
            .handle(
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;
        assert!(trailers.status().is_ok());
    }

    #[tokio::test]
    async fn test_interceptor_successful_trailers_only_response() {
        struct TrailersOnlyHandler;
        impl Handle for TrailersOnlyHandler {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                _tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                // Send no headers or messages; return trailers directly.
                Trailers::new(Ok(()))
            }
        }

        let chain = TrailersOnlyHandler.with_interceptor(StreamValidationInterceptor);
        let mut tx = MockSendStream;
        let rx = ConfigurableMockRecvStream::new(vec![None]);

        let trailers = chain
            .handle(
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;
        assert!(trailers.status().is_ok());
    }

    #[tokio::test]
    async fn test_interceptor_sending_headers_twice() {
        struct DoubleHeadersHandler;
        impl Handle for DoubleHeadersHandler {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                // First headers frame should succeed
                let _ = tx
                    .send(
                        ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                        SendOptions::default(),
                    )
                    .await;

                // Second headers frame violates protocol sequence; pure error-breaking loop/termination
                if tx
                    .send(
                        ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                        SendOptions::default(),
                    )
                    .await
                    .is_err()
                {
                    return Trailers::new(Ok(()));
                }

                Trailers::new(Ok(()))
            }
        }

        let chain = DoubleHeadersHandler.with_interceptor(StreamValidationInterceptor);
        let mut tx = MockSendStream;
        let rx = ConfigurableMockRecvStream::new(vec![None]);

        let trailers = chain
            .handle(
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Internal);
        assert!(err.message().contains("Stream validation error"));
    }

    #[tokio::test]
    async fn test_interceptor_underlying_send_error() {
        struct SendFailureHandler;
        impl Handle for SendFailureHandler {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                // Valid sequence, but underlying transport fails; loop terminates purely on error
                loop {
                    if tx
                        .send(
                            ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                            SendOptions::default(),
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Trailers::new(Ok(()))
            }
        }

        let chain = SendFailureHandler.with_interceptor(StreamValidationInterceptor);
        let mut tx = FailingMockSendStream;
        let rx = ConfigurableMockRecvStream::new(vec![None]);

        let trailers = chain
            .handle(
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Internal);
        assert!(err.message().contains("Stream validation error"));
    }

    #[tokio::test]
    async fn test_interceptor_terminal_receive_error() {
        struct ActiveRecvErrorHandler;
        impl Handle for ActiveRecvErrorHandler {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                _tx: &mut impl SendStream,
                mut rx: impl RecvStream + 'static,
            ) -> Trailers {
                let mut msg = NopRecvMessage;
                while let Some(Ok(())) = rx.next(&mut msg).await {}
                Trailers::new(Ok(()))
            }
        }

        let chain = ActiveRecvErrorHandler.with_interceptor(StreamValidationInterceptor);
        let mut tx = MockSendStream;
        // Stream encounters terminal receive error actively
        let rx = ConfigurableMockRecvStream::new(vec![Some(Err(()))]);

        let trailers = chain
            .handle(
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Internal);
        assert!(err.message().contains("Stream validation error"));
    }

    #[tokio::test]
    async fn test_interceptor_poll_after_done() {
        struct DoneRecvStream;
        impl RecvStream for DoneRecvStream {
            async fn next(&mut self, _msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
                None
            }
        }
        struct PollAfterDoneHandler;
        impl Handle for PollAfterDoneHandler {
            async fn handle(
                &self,
                _h: RequestHeaders,
                _o: CallOptions,
                _tx: &mut impl SendStream,
                mut rx: impl RecvStream + 'static,
            ) -> Trailers {
                let mut msg = NopRecvMessage;
                assert!(rx.next(&mut msg).await.is_none());
                // Polling after None triggers validation error and preemption
                let res = rx.next(&mut msg).await;
                assert!(matches!(res, Some(Err(()))));
                Trailers::new(Ok(()))
            }
        }
        let chain = PollAfterDoneHandler.with_interceptor(StreamValidationInterceptor);
        let mut tx = MockSendStream;
        let rx = DoneRecvStream;
        let trailers = chain
            .handle(
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Internal);
        assert!(err.message().contains("Stream validation error"));
    }

    #[tokio::test]
    async fn test_interceptor_send_message_before_headers() {
        struct MessageBeforeHeadersHandler;
        impl Handle for MessageBeforeHeadersHandler {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                // Invalid sequence: message before headers; pure error-breaking termination
                loop {
                    if tx
                        .send(
                            ServerResponseStreamItem::Message(&()),
                            SendOptions::default(),
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Trailers::new(Ok(()))
            }
        }

        let chain = MessageBeforeHeadersHandler.with_interceptor(StreamValidationInterceptor);
        let mut tx = MockSendStream;
        let rx = ConfigurableMockRecvStream::new(vec![None]);

        let trailers = chain
            .handle(
                RequestHeaders::default(),
                CallOptions::default(),
                &mut tx,
                rx,
            )
            .await;
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Internal);
        assert!(err.message().contains("Stream validation error"));
    }
}
