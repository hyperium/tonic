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

use crate::core::RecvMessage;
use crate::core::ServerResponseStreamItem;
use crate::server::RecvStream;
use crate::server::SendOptions;
use crate::server::SendStream;

/// Enforces proper gRPC semantics on the server sending stream.
pub struct ServerSendStreamValidator<S> {
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

impl<S> ServerSendStreamValidator<S>
where
    S: SendStream,
{
    /// Constructs a new `ServerSendStreamValidator`.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            state: SendStreamState::Init,
        }
    }
}

impl<S> SendStream for ServerSendStreamValidator<S>
where
    S: SendStream,
{
    async fn send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()> {
        if self.state == SendStreamState::Done {
            // Called send after stream completed
            return Err(());
        }

        let next_state = match &item {
            ServerResponseStreamItem::Headers(_) => match self.state {
                SendStreamState::Init => SendStreamState::HeadersSent,
                _ => {
                    // Received multiple headers frames
                    self.state = SendStreamState::Done;
                    return Err(());
                }
            },
            ServerResponseStreamItem::Message(_) => match self.state {
                SendStreamState::HeadersSent | SendStreamState::MessagesSent => {
                    SendStreamState::MessagesSent
                }
                _ => {
                    // Sent message before headers or stream completed
                    self.state = SendStreamState::Done;
                    return Err(());
                }
            },
        };

        let res = self.inner.send(item, options).await;
        match res {
            Ok(()) => self.state = next_state,
            Err(_) => {
                // Underlying stream failed to send
                self.state = SendStreamState::Done;
            }
        }
        res
    }
}

/// Enforces proper gRPC semantics on the server receiving stream.
pub struct ServerRecvStreamValidator<R> {
    inner: R,
    state: RecvStreamState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecvStreamState {
    Init,
    Done,
}

impl<R> ServerRecvStreamValidator<R>
where
    R: RecvStream,
{
    /// Constructs a new `ServerRecvStreamValidator`.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            state: RecvStreamState::Init,
        }
    }
}

impl<R> RecvStream for ServerRecvStreamValidator<R>
where
    R: RecvStream,
{
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
        if self.state == RecvStreamState::Done {
            // Called next after stream completed
            return Some(Err(()));
        }

        let res = self.inner.next(msg).await;

        match res {
            Some(Ok(())) => Some(Ok(())),
            None => {
                self.state = RecvStreamState::Done;
                None
            }
            Some(Err(())) => {
                // Received error from inner stream
                self.state = RecvStreamState::Done;
                Some(Err(()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ResponseHeaders;
    use crate::core::SendMessage;
    use bytes::Buf;
    use bytes::Bytes;

    impl SendMessage for () {
        fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
            Ok(Box::new(Bytes::new()))
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum SendEvent {
        Headers,
        Message,
    }

    struct MockSendStream {
        events: Vec<SendEvent>,
    }

    impl MockSendStream {
        fn new() -> Self {
            Self { events: Vec::new() }
        }
    }

    impl SendStream for MockSendStream {
        async fn send<'a>(
            &mut self,
            item: ServerResponseStreamItem<'a>,
            _options: SendOptions,
        ) -> Result<(), ()> {
            match item {
                ServerResponseStreamItem::Headers(_) => self.events.push(SendEvent::Headers),
                ServerResponseStreamItem::Message(_) => self.events.push(SendEvent::Message),
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_send_validator_valid_full_stream() {
        let mock = MockSendStream::new();
        let mut validator = ServerSendStreamValidator::new(mock);

        assert!(
            validator
                .send(
                    ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                    SendOptions::default()
                )
                .await
                .is_ok()
        );
        assert!(
            validator
                .send(
                    ServerResponseStreamItem::Message(&()),
                    SendOptions::default()
                )
                .await
                .is_ok()
        );

        assert_eq!(
            validator.inner.events,
            vec![SendEvent::Headers, SendEvent::Message]
        );
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

    #[tokio::test]
    async fn test_send_validator_invalid_message_before_headers() {
        let mock = MockSendStream::new();
        let mut validator = ServerSendStreamValidator::new(mock);

        assert!(
            validator
                .send(
                    ServerResponseStreamItem::Message(&()),
                    SendOptions::default()
                )
                .await
                .is_err()
        );
        assert_eq!(validator.state, SendStreamState::Done);
    }

    #[tokio::test]
    async fn test_send_validator_invalid_headers_twice() {
        let mock = MockSendStream::new();
        let mut validator = ServerSendStreamValidator::new(mock);

        assert!(
            validator
                .send(
                    ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                    SendOptions::default()
                )
                .await
                .is_ok()
        );
        assert!(
            validator
                .send(
                    ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                    SendOptions::default()
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_send_validator_state_transitions_to_done_on_error() {
        let mock = FailingMockSendStream;
        let mut validator = ServerSendStreamValidator::new(mock);

        assert!(
            validator
                .send(
                    ServerResponseStreamItem::Headers(ResponseHeaders::default()),
                    SendOptions::default()
                )
                .await
                .is_err()
        );
        assert_eq!(validator.state, SendStreamState::Done);
    }

    struct MockRecvStream {
        items: Vec<Option<Result<(), ()>>>,
        index: usize,
    }

    impl MockRecvStream {
        fn new(items: Vec<Option<Result<(), ()>>>) -> Self {
            Self { items, index: 0 }
        }
    }

    impl RecvStream for MockRecvStream {
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

    struct NopRecvMessage;
    impl RecvMessage for NopRecvMessage {
        fn decode(&mut self, _data: &mut dyn bytes::Buf) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_recv_validator_valid_unary() {
        let mock = MockRecvStream::new(vec![Some(Ok(())), None]);
        let mut validator = ServerRecvStreamValidator::new(mock);
        let mut msg = NopRecvMessage;

        assert!(matches!(validator.next(&mut msg).await, Some(Ok(()))));
        assert!(validator.next(&mut msg).await.is_none());
    }

    #[tokio::test]
    async fn test_recv_validator_empty_stream() {
        let mock = MockRecvStream::new(vec![None]);
        let mut validator = ServerRecvStreamValidator::new(mock);
        let mut msg = NopRecvMessage;

        assert!(validator.next(&mut msg).await.is_none());
    }

    #[tokio::test]
    async fn test_recv_validator_error_after_done() {
        let mock = MockRecvStream::new(vec![None]);
        let mut validator = ServerRecvStreamValidator::new(mock);
        let mut msg = NopRecvMessage;

        assert!(validator.next(&mut msg).await.is_none());
        assert!(matches!(validator.next(&mut msg).await, Some(Err(()))));
    }

    #[tokio::test]
    async fn test_recv_validator_valid_streaming() {
        let mock = MockRecvStream::new(vec![Some(Ok(())), Some(Ok(())), None]);
        let mut validator = ServerRecvStreamValidator::new(mock);
        let mut msg = NopRecvMessage;

        assert!(matches!(validator.next(&mut msg).await, Some(Ok(()))));
        assert!(matches!(validator.next(&mut msg).await, Some(Ok(()))));
        assert!(validator.next(&mut msg).await.is_none());
    }

    #[tokio::test]
    async fn test_recv_validator_terminal_error() {
        let mock = MockRecvStream::new(vec![Some(Err(()))]);
        let mut validator = ServerRecvStreamValidator::new(mock);
        let mut msg = NopRecvMessage;

        assert!(matches!(validator.next(&mut msg).await, Some(Err(()))));

        // Further calls should return the same error
        assert!(matches!(validator.next(&mut msg).await, Some(Err(()))));
    }
}
