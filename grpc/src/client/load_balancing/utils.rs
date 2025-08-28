use crate::client::load_balancing::child_manager::{
    ChildManager, ChildUpdate, ResolverUpdateSharder,
};
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::name_resolution::{Endpoint, ResolverUpdate};
use std::error::Error;
use std::sync::Arc;

/// EndpointSharder shards a resolver update into individual endpoints,
/// with each endpoint serving as the unique identifier for a child.
///
/// The EndpointSharder implements the ResolverUpdateSharder trait,
/// allowing any load-balancing (LB) policy that uses the ChildManager
/// to split a resolver update into individual endpoints, with one endpoint for each child.
pub struct EndpointSharder {
    pub builder: Arc<dyn LbPolicyBuilder>,
}

// Creates a ChildUpdate for each endpoint received.
impl ResolverUpdateSharder<Endpoint> for EndpointSharder {
    fn shard_update(
        &self,
        resolver_update: ResolverUpdate,
    ) -> Result<Box<dyn Iterator<Item = ChildUpdate<Endpoint>>>, Box<dyn Error + Send + Sync>> {
        let mut sharded_endpoints = Vec::new();
        for endpoint in resolver_update.endpoints.unwrap().iter() {
            let child_update = ChildUpdate {
                child_identifier: endpoint.clone(),
                child_policy_builder: self.builder.clone(),
                child_update: ResolverUpdate {
                    attributes: resolver_update.attributes.clone(),
                    endpoints: Ok(vec![endpoint.clone()]),
                    service_config: resolver_update.service_config.clone(),
                    resolution_note: resolver_update.resolution_note.clone(),
                },
            };
            sharded_endpoints.push(child_update);
        }
        Ok(Box::new(sharded_endpoints.into_iter()))
    }
}
