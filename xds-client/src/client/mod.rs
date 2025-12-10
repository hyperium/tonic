//! Client interface through which the user can watch and receive updates for xDS resources.

use crate::client::config::ClientConfig;
use crate::client::watch::ResourceWatcher;
use crate::error::Result;
use crate::resource::Resource;

pub mod config;
pub mod watch;

/// Builder for [`XdsClient`].
#[derive(Debug)]
pub struct XdsClientBuilder {
    _config: ClientConfig,
}

impl XdsClientBuilder {
    /// Create a new builder with the given configuration.
    pub fn new(config: ClientConfig) -> Self {
        Self { _config: config }
    }

    /// Build the client with the given transport and runtime.
    ///
    /// This starts the background worker that manages the ADS stream.
    pub async fn build<T, R>(self, _transport: T, _runtime: R) -> Result<XdsClient> {
        todo!()
    }
}

/// The xDS client.
///
/// This is a handle to the background worker that manages the ADS stream.
/// Cloning this handle creates a new reference to the same worker.
#[derive(Clone, Debug)]
pub struct XdsClient {
    // TODO: add fields as needed
}

impl XdsClient {
    /// Create a new builder.
    pub fn builder(config: ClientConfig) -> XdsClientBuilder {
        XdsClientBuilder::new(config)
    }

    /// Watch a resource by name.
    ///
    /// Returns a [`ResourceWatcher`] that receives events for this resource.
    /// Dropping the watcher automatically unsubscribes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut watcher = client.watch::<Listener>("my-listener");
    /// while let Some(event) = watcher.next().await {
    ///     match event {
    ///         ResourceEvent::Upsert(resource) => {
    ///             println!("Listener updated: {}", resource.name());
    ///         }
    ///         ResourceEvent::Removed { name } => {
    ///             println!("Listener removed: {}", name);
    ///         }
    ///         ResourceEvent::Error(error) => {
    ///             println!("Error watching listener: {}", error);
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if the resource type is not supported.
    ///
    /// # Errors
    /// ```
    pub fn watch<T: Resource>(&self, _name: impl Into<String>) -> ResourceWatcher<T> {
        todo!()
    }
}
