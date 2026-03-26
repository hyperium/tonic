use crate::server::message::AsMut;
use crate::server::stream::{PushStream, PushStreamProducer};
use crate::ServerStatus;

/// A trait for client streaming gRPC methods.
#[trait_variant::make(Send)]
pub trait ClientStreamingMethod: Send {
    type Req: Send;
    type Resp: AsMut + Send;

    /// Handles a client streaming request.
    async fn client_streaming<P>(
        &self,
        req: PushStream<P>,
        resp: <Self::Resp as AsMut>::Mut<'_>,
    ) -> Result<(), ServerStatus>
    where
        P: PushStreamProducer<Item = Self::Req> + Send + 'static;
}
