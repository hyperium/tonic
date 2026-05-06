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

use std::marker::PhantomData;
use std::time::Duration;
use std::time::Instant;

use bytes::Buf;
use grpc::Status;
use grpc::client::CallOptions;
use grpc::client::InvokeOnce;
use grpc::client::RecvStream as ClientRecvStream;
use grpc::client::SendOptions;
use grpc::client::SendStream;
use grpc::client::interceptor::Intercept;
use grpc::client::interceptor::InterceptOnce;
use grpc::client::interceptor::Intercepted;
use grpc::client::interceptor::IntoOnce;
use grpc::client::interceptor::InvokeOnceExt as _;
use grpc::client::stream_util::RecvStreamValidator;
use grpc::core::ClientResponseStreamItem;
use grpc::core::RecvMessage;
use protobuf::AsMut;
use protobuf::Message;
use protobuf::MessageMut;
use protobuf::MessageView;

use crate::ProtoRecvMessage;
use crate::ProtoSendMessage;
use crate::private::Internal;

pub(crate) mod bidi;
pub(crate) mod client_streaming;
pub(crate) mod server_streaming;
pub(crate) mod unary;

/// Allows sending streaming RPC protobuf request messages.
///
/// If this is dropped by the client before the RPC has completed, the client
/// enters a half-closed state which the server may observe.
pub struct GrpcStreamingRequest<M, Tx> {
    tx: Tx,
    _phantom: PhantomData<M>,
}

impl<M, Tx> GrpcStreamingRequest<M, Tx>
where
    Tx: SendStream,
    M: Message,
    for<'b> M::View<'b>: MessageView<'b>,
{
    fn new(tx: Tx) -> Self {
        Self {
            tx,
            _phantom: PhantomData,
        }
    }

    /// Sends `message` on the stream.  Will block if flow control does not
    /// allow for sending the request message.  Returns an error if the stream
    /// has ended.  In this case, the application should call the associated
    /// `GrpcStreamingResponse::status` method to determine the stream's final
    /// status.
    ///
    /// Note: success does *not* indicate successful transmission of the request
    /// or successful receipt of the request by the server.  Success only
    /// indicates that the stream has not yet terminated.
    pub async fn send(&mut self, message: M) -> Result<(), ()> {
        self.tx
            .send(
                &ProtoSendMessage::from_view(&message),
                SendOptions::default(),
            )
            .await
    }

    /// Sends a "half close" signal to the server to indicate the client is done
    /// sending by dropping self.  It is safe to just drop(self) instead; this
    /// method is provided to be explicit.
    pub fn close(self) {}
}

/// Provides a streaming RPC's protobuf response messages and status.
///
/// If this is dropped by the client before the RPC has completed, the call will
/// be cancelled.
pub struct GrpcStreamingResponse<M, Rx> {
    rx: RecvStreamValidator<Rx>,
    status: Option<Status>,
    _phantom: PhantomData<M>,
}

impl<M, Rx> GrpcStreamingResponse<M, Rx>
where
    Rx: ClientRecvStream,
    M: Message,
    for<'b> M::Mut<'b>: MessageMut<'b>,
{
    fn new(rx: Rx) -> Self {
        Self {
            rx: RecvStreamValidator::new(rx, false),
            status: None,
            _phantom: PhantomData,
        }
    }

    /// Receives the next response message from the stream into `res` and
    /// returns Ok on success or Err if the stream has ended.
    pub async fn recv_into(&mut self, res: &mut impl AsMut<MutProxied = M>) -> Result<(), ()> {
        let mut res_view = ProtoRecvMessage::from_mut(res);
        let mut i = self.rx.next(&mut res_view).await;

        // Ignore headers and request the next item.
        if matches!(i, ClientResponseStreamItem::Headers(_)) {
            i = self.rx.next(&mut res_view).await;
        }
        drop(res_view);

        // Note that because we use the RecvStreamValidator, we know the stream
        // will follow the protocol; this means:
        //
        // 1. There will always be a Trailers message at the end of the stream.
        // 2. If we receive Trailers, we will only ever receive StreamClosed.
        match i {
            ClientResponseStreamItem::Headers(_) => unreachable!(),
            ClientResponseStreamItem::Message => Ok(()),
            ClientResponseStreamItem::Trailers(trailers) => {
                self.status = Some(trailers.into_status());
                Err(())
            }
            ClientResponseStreamItem::StreamClosed => Err(()),
        }
    }

    /// Returns the next response message from the stream, or `None` if the
    /// stream has completed.
    pub async fn recv(&mut self) -> Option<M> {
        let mut res = M::default();
        match self.recv_into(&mut res).await {
            Ok(_) => Some(res),
            Err(_) => None,
        }
    }

    /// Returns the final status of the stream, draining any unread messages as
    /// needed.
    pub async fn status(mut self) -> Status {
        if let Some(status) = self.status.take() {
            // We encountered a status while handling a call to next.
            status
        } else {
            // Drain the stream until we find trailers.
            let mut nop_msg = NopRecvMessage;
            loop {
                let i = self.rx.next(&mut nop_msg).await;
                if let ClientResponseStreamItem::Trailers(t) = i {
                    return t.into_status();
                }
            }
        }
    }
}

struct NopRecvMessage;
impl RecvMessage for NopRecvMessage {
    fn decode(&mut self, _data: &mut dyn Buf) -> Result<(), String> {
        Ok(())
    }
}

/// Common trait for configuring an RPC call.  Implemented by all gRPC protobuf
/// call builders.
pub trait CallBuilder<C: InvokeOnce>: Sized {
    /// Applies the timeout `t` to the call.
    fn with_timeout(mut self, t: Duration) -> Self {
        self.args_mut(Internal).set_deadline(Instant::now() + t);
        self
    }

    /// Attaches a multi-use `interceptor` to the call.
    fn with_interceptor<I: Intercept<C>>(
        self,
        interceptor: I,
    ) -> Self::Builder<Intercepted<C, IntoOnce<I>>> {
        self.rebuild(|c| c.with_interceptor(interceptor), Internal)
    }

    /// Attaches a single-use `interceptor` to the call.
    fn with_once_interceptor<I: InterceptOnce<C>>(
        self,
        interceptor: I,
    ) -> Self::Builder<Intercepted<C, I>> {
        self.rebuild(|c| c.with_once_interceptor(interceptor), Internal)
    }

    /// Defines the builder type that `rebuild` produces as its output.
    /// This type is for internal use only.
    #[doc(hidden)]
    type Builder<NewC: InvokeOnce>: CallBuilder<NewC>;

    /// Rebuilds the current builder into a new one using the conversion
    /// function.
    #[doc(hidden)]
    fn rebuild<NewC: InvokeOnce>(
        self,
        f: impl FnOnce(C) -> NewC,
        _: Internal,
    ) -> Self::Builder<NewC>;

    /// Returns the mutable call options contained in the builder.
    #[doc(hidden)]
    fn args_mut(&mut self, _: Internal) -> &mut CallOptions;
}
