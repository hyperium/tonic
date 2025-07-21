use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use once_cell::sync::Lazy;

use super::LbPolicyBuilder;

/// A registry to store and retrieve LB policies.  LB policies are indexed by
/// their names.
pub struct LbPolicyRegistry {
    m: Arc<Mutex<HashMap<String, Arc<dyn LbPolicyBuilder>>>>,
}

impl LbPolicyRegistry {
    /// Construct an empty LB policy registry.
    pub fn new() -> Self {
        Self { m: Arc::default() }
    }
    /// Add a LB policy into the registry.
    pub(crate) fn add_builder(&self, builder: impl LbPolicyBuilder + 'static) {
        self.m
            .lock()
            .unwrap()
            .insert(builder.name().to_string(), Arc::new(builder));
    }
    /// Retrieve a LB policy from the registry, or None if not found.
    pub(crate) fn get_policy(&self, name: &str) -> Option<Arc<dyn LbPolicyBuilder>> {
        self.m.lock().unwrap().get(name).cloned()
    }
}

impl Default for LbPolicyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The registry used if a local registry is not provided to a channel or if it
/// does not exist in the local registry.
pub static GLOBAL_LB_REGISTRY: Lazy<LbPolicyRegistry> = Lazy::new(LbPolicyRegistry::new);
