use crate::server::message::AsView;
use crate::server::stream::{PushStreamConsumer, PushStreamWriter};
use crate::ServerStatus;

/// A trait for server streaming gRPC methods.
#[trait_variant::make(Send)]
pub trait ServerStreamingMethod: Send {
    type Req: AsView + Send;
    type Resp: Send;

    /// Handles a server streaming request.
    async fn server_streaming<C>(
        &self,
        req: <Self::Req as AsView>::View<'_>,
        writer: PushStreamWriter<C>,
    ) -> Result<(), ServerStatus>
    where
        C: PushStreamConsumer<Self::Resp> + Send + 'static;
}
