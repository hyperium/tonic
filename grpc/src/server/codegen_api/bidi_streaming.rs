use crate::server::stream::{PushStream, PushStreamConsumer, PushStreamProducer, PushStreamWriter};
use crate::ServerStatus;

/// A trait for bidirectional streaming gRPC methods.
#[trait_variant::make(Send)]
pub trait BidiStreamingMethod: Send {
    type Req: Send;
    type Resp: Send;

    /// Handles a bidirectional streaming request.
    async fn bidi_streaming<P, C>(
        &self,
        req: PushStream<P>,
        writer: PushStreamWriter<C>,
    ) -> Result<(), ServerStatus>
    where
        P: PushStreamProducer<Item = Self::Req> + Send + 'static,
        C: PushStreamConsumer<Self::Resp> + Send + 'static;
}
