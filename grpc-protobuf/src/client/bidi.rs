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
use grpc::core::RequestHeaders;
use protobuf::ClearAndParse;
use protobuf::Message;
use protobuf::MessageMut;
use protobuf::MessageView;

use crate::CallBuilder;
use crate::GrpcStreamingRequest;
use crate::GrpcStreamingResponse;
use crate::private::Internal;

/// Configures a bidirectional call for gRPC Protobuf.  Implements
/// [`std::future::IntoFuture`] which begins the call and resolves to a pair of
/// send/receive types: ([`GrpcStreamingRequest`], [`GrpcStreamingResponse`]).
/// Implements [`crate::CallBuilder`] to provide common RPC configuration methods.
pub struct BidiCallBuilder<'a, C, Req, Res> {
    channel: C,
    method: String,
    args: CallOptions,
    _phantom: PhantomData<&'a (Req, Res)>,
}

impl<'a, C, Req, Res> BidiCallBuilder<'a, C, Req, Res> {
    /// Creates a new [BidiCallBuilder] for configuring a bidirectional call.
    pub fn new(channel: C, method: impl Into<String>) -> Self {
        Self {
            channel,
            method: method.into(),
            args: Default::default(),
            _phantom: PhantomData,
        }
    }
}

impl<'a, C, Req, Res> IntoFuture for BidiCallBuilder<'a, C, Req, Res>
where
    C: InvokeOnce + 'a,
    // Req is a proto message. (Ideally we could just require "Message" and
    // protobuf would automatically include the rest.  For now we need the
    // HRTBs.)
    Req: Message,
    for<'b> Req::View<'b>: MessageView<'b>,
    // Res is a proto message. (Ideally we could just require "Message" and
    // protobuf would automatically include the rest.  For now we need the
    // HRTBs.)
    Res: Message + ClearAndParse,
    for<'b> Res::Mut<'b>: MessageMut<'b>,
{
    type Output = (
        GrpcStreamingRequest<Req, C::SendStream>,
        GrpcStreamingResponse<Res, C::RecvStream>,
    );
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let headers = RequestHeaders::new().with_method_name(self.method);
            let (tx, rx) = self.channel.invoke_once(headers, self.args).await;
            (
                GrpcStreamingRequest::new(tx),
                GrpcStreamingResponse::new(rx),
            )
        })
    }
}

impl<'a, C: InvokeOnce, Req, Res> CallBuilder<C> for BidiCallBuilder<'a, C, Req, Res> {
    type Builder<NewC: InvokeOnce> = BidiCallBuilder<'a, NewC, Req, Res>;
    fn rebuild<NewC>(
        self,
        f: impl FnOnce(C) -> NewC,
        _: Internal,
    ) -> BidiCallBuilder<'a, NewC, Req, Res> {
        BidiCallBuilder {
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
