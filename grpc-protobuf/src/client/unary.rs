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

use grpc::Status;
use grpc::StatusErr;
use grpc::client::CallOptions;
use grpc::client::InvokeOnce;
use grpc::client::RecvStream as _;
use grpc::client::SendOptions;
use grpc::client::SendStream as _;
use grpc::client::stream_util::RecvStreamValidator;
use grpc::core::ClientResponseStreamItem;
use grpc::core::RequestHeaders;
use protobuf::AsMut;
use protobuf::AsView;
use protobuf::ClearAndParse;
use protobuf::Message;
use protobuf::MessageView;
use protobuf::Proxied;

use crate::CallBuilder;
use crate::ProtoRecvMessage;
use crate::ProtoSendMessage;
use crate::client::Internal;

/// Configures a unary call for gRPC Protobuf.  Implements `IntoFuture` which
/// performs the call and resolves to the response as a `Result<Res, Status>`.
/// Implements `CallBuilder` to provide common RPC configuration methods.
pub struct UnaryCallBuilder<'a, C, ReqMsgView, Res> {
    channel: C,
    method: String,
    req: ReqMsgView,
    args: CallOptions,
    _phantom: PhantomData<&'a Res>,
}

impl<'a, C, ReqMsgView, Res> UnaryCallBuilder<'a, C, ReqMsgView, Res>
where
    C: InvokeOnce,
{
    /// Creates a new UnaryCallBuilder for configuring a unary call.
    pub fn new(channel: C, method: impl Into<String>, req: ReqMsgView) -> Self {
        Self {
            channel,
            req,
            method: method.into(),
            args: Default::default(),
            _phantom: PhantomData,
        }
    }

    /// Performs the call immediately, setting `res` with the response message
    /// and returning the status of the call.
    pub async fn with_response_message(self, res: &mut impl AsMut<MutProxied = Res>) -> Status
    where
        // ReqMsgView is a proto message view. (Ideally we could just require
        // "AsView" and protobuf would automatically include the rest.)
        ReqMsgView: AsView + Send + Sync + 'a,
        <ReqMsgView as AsView>::Proxied: Message,
        for<'b> <<ReqMsgView as AsView>::Proxied as Proxied>::View<'b>: MessageView<'b>,
        // Res is a proto message. (Ideally we could just require "Message" and
        // protobuf would automatically include the rest.)
        Res: Message,
        for<'b> Res::Mut<'b>: ClearAndParse + Send + Sync,
    {
        let headers = RequestHeaders::new().with_method_name(self.method);
        let (mut tx, rx) = self.channel.invoke_once(headers, self.args).await;
        let mut rx = RecvStreamValidator::new(rx, true);
        let req = &ProtoSendMessage::from_view(&self.req);
        let _ = tx.send(req, SendOptions::new().with_final_msg(true)).await;
        let mut res = ProtoRecvMessage::from_mut(res);
        loop {
            let i = rx.next(&mut res).await;
            if let ClientResponseStreamItem::Trailers(t) = i {
                return t.status().clone();
            }
        }
    }
}

impl<'a, C, ReqMsgView, Res> IntoFuture for UnaryCallBuilder<'a, C, ReqMsgView, Res>
where
    C: InvokeOnce + 'a,
    // ReqMsgView is a proto message view. (Ideally we could just require
    // "AsView" and protobuf would automatically include the rest.  For now we
    // need the HRTBs.)
    ReqMsgView: AsView + Send + Sync + 'a,
    <ReqMsgView as AsView>::Proxied: Message,
    for<'b> <<ReqMsgView as AsView>::Proxied as Proxied>::View<'b>: MessageView<'b>,
    // Res is a proto message. (Ideally we could just require "Message" and
    // protobuf would automatically include the rest.  For now we need the
    // HRTBs.)
    Res: Message,
    for<'b> Res::Mut<'b>: ClearAndParse + Send + Sync,
{
    type Output = Result<Res, StatusErr>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let mut res = Res::default();
            self.with_response_message(&mut res).await?;
            Ok(res)
        })
    }
}

impl<'a, C: InvokeOnce, ReqMsgView, Res> CallBuilder<C>
    for UnaryCallBuilder<'a, C, ReqMsgView, Res>
{
    type Builder<NewC: InvokeOnce> = UnaryCallBuilder<'a, NewC, ReqMsgView, Res>;
    fn rebuild<NewC>(
        self,
        f: impl FnOnce(C) -> NewC,
        _: Internal,
    ) -> UnaryCallBuilder<'a, NewC, ReqMsgView, Res> {
        UnaryCallBuilder {
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
