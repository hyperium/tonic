use std::future::Future;
use std::pin::Pin;
use tower::{discover::Change, BoxError};

use crate::xds::route::{RouteDecision, RouteInput};

pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(crate) type BoxDiscover<E, C> =
    Pin<Box<dyn futures::Stream<Item = Result<Change<E, C>, BoxError>> + Send>>;

/// Trait for routing requests to clusters based on xDS routing configurations.
pub(crate) trait XdsRouter: Send + Sync + 'static {
    fn route(&self, input: &RouteInput<'_>) -> BoxFut<RouteDecision>;
}

/// Trait for discovering cluster endpoints based on xDS cluster configurations.
pub(crate) trait XdsClusterDiscovery<E, C>: Send + Sync + 'static {
    fn discover_cluster(&self, cluster_name: &str) -> BoxDiscover<E, C>;
}

/// Combined trait for xDS management (routing + load balancing).
/// Automatically implemented for any type that implements both `XdsRouter` and `XdsClusterDiscovery`.
#[allow(dead_code)]
pub(crate) trait XdsManager<E, C>: XdsRouter + XdsClusterDiscovery<E, C> {}

impl<T, E, C> XdsManager<E, C> for T where T: XdsRouter + XdsClusterDiscovery<E, C> {}
