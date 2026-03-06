use crate::{
    server::call::{HandlerCallOptions, Lazy, Outgoing, StreamingRequest, StreamingResponseWriter},
    server::message::AsMut,
    server::method_handler::RpcResponseHolder,
    server::stream::PushStreamProducer,
    Status,
};

/// A unified trait for all streaming gRPC methods.
#[trait_variant::make(Send)]
pub trait MessageStreamHandler: Send {
    type Req: Send + AsMut;
    type Resp: Send + AsMut;

    /// The response holder type produced by this handler.
    type ResponseHolder: RpcResponseHolder<Self::Resp>;

    /// Handles a streaming request.
    /// The request stream is modelled as a tuple of (headers, stream of lazy messages)
    /// The writer is modelled as a compile time state machine capable of writing headers, messages and trailers.
    /// The messages are put inside a container.
    async fn call<P, W, L>(
        &self,
        options: HandlerCallOptions,
        req: StreamingRequest<P>,
        writer: W,
    ) -> Result<(), Status>
    where
        P: PushStreamProducer<Item = L> + Send + 'static,
        W: StreamingResponseWriter<Outgoing<Self::ResponseHolder>> + Send,
        L: Lazy<Self::Req>,
        <W as StreamingResponseWriter<Outgoing<Self::ResponseHolder>>>::MessageWriter: 'static;
}
