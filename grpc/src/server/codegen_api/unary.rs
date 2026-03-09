use crate::server::message::{AsMut, AsView};
use crate::{ServerStatus, Status};

/// A trait for unary gRPC methods.
#[trait_variant::make(Send)]
pub trait UnaryMethod: Send {
    type Req: AsView + Send;
    type Resp: AsMut + Send;

    /// Handles a unary request.
    async fn unary(
        &self,
        req: <Self::Req as AsView>::View<'_>,
        resp: <Self::Resp as AsMut>::Mut<'_>,
    ) -> Result<(), ServerStatus>;
}
