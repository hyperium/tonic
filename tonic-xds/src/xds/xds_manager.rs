use std::future::Future;
use std::pin::Pin;
use tower::{BoxError, discover::Change};

use crate::xds::route::{RouteInput, RouteDecision};

pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(crate) type BoxDiscover<E, C> = Pin<Box<dyn futures::Stream<Item = Result<Change<E, C>, BoxError>> + Send>>;

/// Trait for xDS routing and service discovery.
/// Implementors of this trait should subscribe to xDS resources and
/// provide routing decisions and cluster discovery
pub(crate) trait XdsManager<E, C>: Send + Sync + 'static {
    // Returns a routing decision based on the provided route input.
    fn route(&self, input: &RouteInput<'_>) -> BoxFut<RouteDecision>;
    // Discovers the cluster with the given name, returning a stream of endpoint changes.
    fn discover_cluster(&self, cluster_name: &str) -> BoxDiscover<E, C>;
}