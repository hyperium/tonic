#![allow(dead_code)]
/// Represents the input for xDS routing decisions.
pub(crate) struct RouteInput<'a> {
    /// The authority (host) of the request URI. This is used for sending LDS request to
    /// fetch the routing configurations from xDS server.
    pub authority: &'a str,
    /// The HTTP headers of the request. These can be used for header-based routing decisions.
    pub headers: &'a http::HeaderMap,
}

/// Represents the routing decision made by the xDS routing layer.
#[derive(Clone)]
pub(crate) struct RouteDecision {
    /// The name of the cluster to which the request should be routed.
    pub cluster: String,
}
