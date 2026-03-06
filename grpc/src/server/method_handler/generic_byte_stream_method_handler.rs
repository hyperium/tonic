use crate::server::call::{
    HandlerCallOptions, Incoming, StreamingRequest, StreamingResponseWriter,
};
use crate::server::stream::PushStreamProducer;
use crate::Status;
use bytes::Buf;

/// A method handler that processes raw bytes.
#[trait_variant::make(Send)]
pub trait GenericByteStreamMethodHandler: Send + Sync {
    type RespB: Buf + Send + 'static;

    async fn call<ReqB, P, W>(
        &self,
        options: HandlerCallOptions,
        req: StreamingRequest<P>,
        resp: W,
    ) -> Result<(), Status>
    where
        ReqB: Buf + Send,
        W: StreamingResponseWriter<Self::RespB> + Send,
        <W as StreamingResponseWriter<Self::RespB>>::MessageWriter: 'static,
        P: PushStreamProducer<Item = Incoming<ReqB>> + Send + 'static;
}
