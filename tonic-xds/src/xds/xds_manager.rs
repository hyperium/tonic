use std::future::Future;
use std::pin::Pin;

use envoy_types::pb::envoy::config::route::v3::Route;

use crate::xds::route::{RouteInput, RouteDecision};
use tower::discover::Change;
use tower::BoxError;

pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub(crate) type BoxDiscover<E, C> = Pin<Box<dyn futures::Stream<Item = Result<Change<E, C>, BoxError>> + Send>>;

pub(crate) trait XdsManager<E, C>: Send + Sync + 'static {
    fn route(&self, input: &RouteInput<'_>) -> BoxFut<RouteDecision>;
    fn discover_cluster(&self, cluster_name: &str) -> BoxDiscover<E, C>;
}