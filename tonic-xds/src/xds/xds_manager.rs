use std::future::Future;
use std::pin::Pin;
use tower::{discover::Change, BoxError};

use crate::xds::route::{RouteDecision, RouteInput};

pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(crate) type BoxDiscover<Endpoint, S> =
    Pin<Box<dyn futures::Stream<Item = Result<Change<Endpoint, S>, BoxError>> + Send>>;

/// Trait for routing requests to clusters based on xDS routing configurations.
pub(crate) trait XdsRouter: Send + Sync + 'static {
    fn route(&self, input: &RouteInput<'_>) -> BoxFut<RouteDecision>;
}

/// Trait for discovering cluster endpoints based on xDS cluster configurations.
pub(crate) trait XdsClusterDiscovery<Endpoint, S>: Send + Sync + 'static {
    fn discover_cluster(&self, cluster_name: &str) -> BoxDiscover<Endpoint, S>;
}

/// Combined trait for xDS management (routing + load balancing).
/// Automatically implemented for any type that implements both `XdsRouter` and `XdsClusterDiscovery`.
#[allow(dead_code)]
pub(crate) trait XdsManager<Endpoint, S>:
    XdsRouter + XdsClusterDiscovery<Endpoint, S>
{
}

impl<T, Endpoint, S> XdsManager<Endpoint, S> for T where
    T: XdsRouter + XdsClusterDiscovery<Endpoint, S>
{
}
