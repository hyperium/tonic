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
use grpc::client::SendOptions;
use grpc::client::SendStream as _;
use grpc::client::stream_util::RecvStreamValidator;
use grpc::core::RequestHeaders;
use protobuf::AsView;
use protobuf::ClearAndParse;
use protobuf::Message;
use protobuf::MessageMut;
use protobuf::MessageView;
use protobuf::Proxied;

use crate::CallBuilder;
use crate::GrpcStreamingResponse;
use crate::ProtoSendMessage;
use crate::client::Internal;

/// Configures a server-streaming call for gRPC Protobuf.  Implements
/// `IntoFuture` which begins the call and resolves to a `GrpcStreamingResponse`
/// which allows for receiving response messages and the status.  Implements
/// `CallBuilder` to provide common RPC configuration methods.
pub struct ServerStreamingCallBuilder<'a, C, ReqMsgView, Res> {
    channel: C,
    method: String,
    args: CallOptions,
    req: ReqMsgView,
    _phantom: PhantomData<&'a Res>,
}

impl<'a, C, ReqMsgView, Res> ServerStreamingCallBuilder<'a, C, ReqMsgView, Res> {
    pub fn new(channel: C, method: impl Into<String>, req: ReqMsgView) -> Self {
        Self {
            channel,
            req,
            method: method.into(),
            args: Default::default(),
            _phantom: PhantomData,
        }
    }
}

impl<'a, C, ReqMsgView, Res> IntoFuture for ServerStreamingCallBuilder<'a, C, ReqMsgView, Res>
where
    C: InvokeOnce + 'a,
    // ReqMsgView is a proto message view. (Ideally we could just require
    // "AsView" and protobuf would automatically include the rest.)
    ReqMsgView: AsView + Send + Sync + 'a,
    <ReqMsgView as AsView>::Proxied: Message,
    for<'b> <<ReqMsgView as AsView>::Proxied as Proxied>::View<'b>: MessageView<'b>,
    // Res is a proto message. (Ideally we could just require "Message" and
    // protobuf would automatically include the rest.)
    Res: Message + ClearAndParse,
    for<'b> Res::Mut<'b>: MessageMut<'b>,
{
    type Output = GrpcStreamingResponse<Res, RecvStreamValidator<C::RecvStream>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let headers = RequestHeaders::new().with_method_name(self.method);
            let (mut tx, rx) = self.channel.invoke_once(headers, self.args).await;
            let rx = RecvStreamValidator::new(rx, false);
            let req = &ProtoSendMessage::from_view(&self.req);
            let _ = tx.send(req, SendOptions::new().with_final_msg(true)).await;
            GrpcStreamingResponse::new(rx)
        })
    }
}

impl<'a, C: InvokeOnce, ReqMsgView, Res> CallBuilder<C>
    for ServerStreamingCallBuilder<'a, C, ReqMsgView, Res>
{
    type Builder<NewC: InvokeOnce> = ServerStreamingCallBuilder<'a, NewC, ReqMsgView, Res>;
    fn rebuild<NewC>(
        self,
        f: impl FnOnce(C) -> NewC,
        _: Internal,
    ) -> ServerStreamingCallBuilder<'a, NewC, ReqMsgView, Res> {
        ServerStreamingCallBuilder {
            channel: f(self.channel),
            method: self.method,
            req: self.req,
            args: self.args,
            _phantom: PhantomData,
        }
    }
    fn args_mut(&mut self, _: Internal) -> &mut CallOptions {
        &mut self.args
    }
}
