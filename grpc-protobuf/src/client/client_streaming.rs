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
use std::pin::Pin;

use grpc::client::CallOptions;
use grpc::client::InvokeOnce;
use grpc::client::RecvStream;
use grpc::client::RequestHeaders;
use grpc::client::ResponseStreamItem;
use grpc::client::SendOptions;
use grpc::client::SendStream;
use grpc::client::stream_util::RecvStreamValidator;
use grpc::core::StreamEnded;
use protobuf::AsMut;
use protobuf::AsView;
use protobuf::Message;

use crate::ProtoRecvMessage;
use crate::ProtoSendMessage;
use crate::Status;
use crate::StatusOr;
use crate::client::CallBuilder;
use crate::client::Internal;
use crate::trailers_conv::status_from_trailers;

/// Configures a client-streaming call for gRPC Protobuf.  Implements
/// [`IntoFuture`] which begins the call and resolves to a
/// [`ClientStreamingCall`] which allows sending request messages and receiving
/// the response when done. Implements [`CallBuilder`] to provide common RPC
/// configuration methods.
pub struct ClientStreamingCallBuilder<'a, C, Req, Res> {
    channel: C,
    method: String,
    args: CallOptions,
    _phantom: PhantomData<&'a (Req, Res)>,
}

impl<'a, C, Req, Res> ClientStreamingCallBuilder<'a, C, Req, Res>
where
    C: InvokeOnce,
{
    pub fn new(channel: C, method: impl Into<String>) -> Self {
        Self {
            channel,
            method: method.into(),
            args: Default::default(),
            _phantom: PhantomData,
        }
    }
}

impl<'a, C, Req, Res> IntoFuture for ClientStreamingCallBuilder<'a, C, Req, Res>
where
    C: InvokeOnce + 'a,
    Req: Message,
    Res: Message,
{
    type Output = ClientStreamingCall<'a, C, Req, Res>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let headers = RequestHeaders::new().with_method_name(self.method);
            let (tx, rx) = self.channel.invoke_once(headers, self.args).await;

            ClientStreamingCall {
                tx,
                rx: RecvStreamValidator::new(rx, true),
                _phantom: PhantomData,
            }
        })
    }
}

impl<'a, C: InvokeOnce, Req, Res> CallBuilder<C> for ClientStreamingCallBuilder<'a, C, Req, Res> {
    type Builder<NewC: InvokeOnce> = ClientStreamingCallBuilder<'a, NewC, Req, Res>;
    fn rebuild<NewC>(
        self,
        f: impl FnOnce(C) -> NewC,
        _: Internal,
    ) -> ClientStreamingCallBuilder<'a, NewC, Req, Res> {
        ClientStreamingCallBuilder {
            channel: f(self.channel),
            method: self.method,
            args: self.args,
            _phantom: PhantomData,
        }
    }
    fn args_mut(&mut self, _: Internal) -> &mut CallOptions {
        &mut self.args
    }
}

/// Implements the client-side logic for a client-streaming RPC.  Implements
/// `IntoFuture` which completes the call and resolves to the response as a
/// `Result<Res, Status>`.
pub struct ClientStreamingCall<'a, C: InvokeOnce, Req, Res> {
    tx: C::SendStream,
    rx: RecvStreamValidator<C::RecvStream>,
    _phantom: PhantomData<&'a (Req, Res)>,
}

impl<'a, C, Req, Res> ClientStreamingCall<'a, C, Req, Res>
where
    C: InvokeOnce + 'a,
    Req: Message,
    Res: Message,
{
    /// Sends `message` on the stream.  Will block if flow control does not
    /// allow for sending the request message.  Returns an error if the stream
    /// has ended.  In this case, the application should call
    /// `with_response_message` or await the `ClientStreamingCall` to receive
    /// the server's response.
    ///
    /// Note: success does *not* indicate successful transmission of the request
    /// or successful receipt of the request by the server.  Success only
    /// indicates that the stream has not yet terminated.
    pub async fn send(&mut self, message: impl AsView<Proxied = Req>) -> Result<(), StreamEnded> {
        let msg = ProtoSendMessage::from_view(&message);
        self.tx.send(&msg, SendOptions::default()).await
    }

    /// Completes the RPC, receiving the optional response message into `res` if
    /// the returned Status is StatusCode::Ok.
    pub async fn with_response_message(self, res: &mut impl AsMut<MutProxied = Res>) -> Status {
        let Self { tx, mut rx, .. } = self;
        drop(tx);
        let mut res = ProtoRecvMessage::from_mut(res);
        loop {
            let i = rx.recv(&mut res).await;
            if let ResponseStreamItem::Trailers(t) = i {
                return status_from_trailers(t);
            }
        }
    }

    pub async fn close_and_recv(self) -> StatusOr<Res> {
        let mut res = Res::default();
        self.with_response_message(&mut res).await?;
        Ok(res)
    }
}
