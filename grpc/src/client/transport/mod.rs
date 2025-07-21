use crate::service::Service;

mod registry;

use ::tonic::async_trait;
pub use registry::{TransportRegistry, GLOBAL_TRANSPORT_REGISTRY};

#[async_trait]
pub trait Transport: Send + Sync {
    async fn connect(&self, address: String) -> Result<Box<dyn ConnectedTransport>, String>;
}

#[async_trait]
pub trait ConnectedTransport: Service {
    async fn disconnected(&self);
}
