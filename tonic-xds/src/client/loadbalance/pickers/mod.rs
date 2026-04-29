pub(crate) mod p2c;

use indexmap::IndexMap;

use crate::client::endpoint::EndpointAddress;

/// Trait for selecting a channel to handle a request.
///
/// Generic over `S` (the channel type in the ready set) and `Req` (the request).
/// The picker only needs to observe `S`'s load — it doesn't depend on any
/// specific channel state type.
pub(crate) trait ChannelPicker<S, Req> {
    fn pick<'a>(&self, req: &Req, ready: &'a IndexMap<EndpointAddress, S>) -> Option<&'a S>;
}
