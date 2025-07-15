use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use super::Transport;

/// A registry to store and retrieve transports.  Transports are indexed by
/// the address type they are intended to handle.
#[derive(Clone)]
pub struct TransportRegistry {
    m: Arc<Mutex<HashMap<String, Arc<dyn Transport>>>>,
}

impl std::fmt::Debug for TransportRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = self.m.lock().unwrap();
        for key in m.keys() {
            write!(f, "k: {:?}", key)?
        }
        Ok(())
    }
}

impl TransportRegistry {
    /// Construct an empty name resolver registry.
    pub fn new() -> Self {
        Self { m: Arc::default() }
    }

    /// Add a name resolver into the registry.
    pub fn add_transport(&self, address_type: &str, transport: impl Transport + 'static) {
        self.m
            .lock()
            .unwrap()
            .insert(address_type.to_string(), Arc::new(transport));
    }

    /// Retrieve a name resolver from the registry, or None if not found.
    pub fn get_transport(&self, address_type: &str) -> Result<Arc<dyn Transport>, String> {
        self.m
            .lock()
            .unwrap()
            .get(address_type)
            .ok_or(format!(
                "no transport found for address type {address_type}"
            ))
            .cloned()
    }
}

impl Default for TransportRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The registry used if a local registry is not provided to a channel or if it
/// does not exist in the local registry.
static GLOBAL_TRANSPORT_REGISTRY: OnceLock<TransportRegistry> = OnceLock::new();

/// Global registry for resolver builders.
pub fn global_registry() -> &'static TransportRegistry {
    GLOBAL_TRANSPORT_REGISTRY.get_or_init(TransportRegistry::new)
}
