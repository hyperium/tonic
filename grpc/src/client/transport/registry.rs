use super::Transport;
use std::sync::{Arc, LazyLock, Mutex};
use std::{collections::HashMap, fmt::Debug};

/// A registry to store and retrieve transports.  Transports are indexed by
/// the address type they are intended to handle.
#[derive(Default, Clone)]
pub(crate) struct TransportRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<dyn Transport>>>>,
}

impl Debug for TransportRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = self.inner.lock().unwrap();
        for key in m.keys() {
            write!(f, "k: {key:?}")?
        }
        Ok(())
    }
}

impl TransportRegistry {
    /// Construct an empty name resolver registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add a transport into the registry.
    pub(crate) fn add_transport(&self, address_type: &str, transport: impl Transport + 'static) {
        self.inner
            .lock()
            .unwrap()
            .insert(address_type.to_string(), Arc::new(transport));
    }

    /// Retrieve a name resolver from the registry, or None if not found.
    pub(crate) fn get_transport(&self, address_type: &str) -> Result<Arc<dyn Transport>, String> {
        self.inner
            .lock()
            .unwrap()
            .get(address_type)
            .ok_or(format!(
                "no transport found for address type {address_type}"
            ))
            .cloned()
    }
}

/// The registry used if a local registry is not provided to a channel or if it
/// does not exist in the local registry.
pub(crate) static GLOBAL_TRANSPORT_REGISTRY: LazyLock<TransportRegistry> =
    LazyLock::new(TransportRegistry::new);
