use std::net::SocketAddr;

/// This enumeration represents the different
/// fields of `Ctx`.
///
/// It is used with `Server` to construct the
/// context that is passed to the request.
#[derive(Clone, Copy, Debug)]
pub enum CtxField {
    /// Add the peer address to the context.
    PeerAddr,
}

/// A context passed to the request for use
/// in interceptors.
#[derive(Default, Clone, Copy, Debug)]
pub struct Ctx {
    /// The peer's IP (v4 or v6) address.
    pub peer_addr: Option<SocketAddr>,
}
