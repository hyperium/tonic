//! Provides abstraction for xDS resources.

use crate::error::Result;
use bytes::Bytes;
use std::any::Any;
use std::sync::Arc;

#[cfg(feature = "codegen-prost")]
pub mod prost;

/// A type URL identifying an xDS resource type.
///
/// Format: `type.googleapis.com/<resource_type>`
/// e.g. `type.googleapis.com/envoy.config.listener.v3.Listener`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeUrl(&'static str);

impl TypeUrl {
    /// Create a new type URL from a static string.
    pub const fn new(url: &'static str) -> Self {
        Self(url)
    }

    /// Returns the type URL as a string slice.
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

/// Trait for xDS resources.
///
/// # Validation
///
/// The `decode` method should:
/// - Parse the raw bytes based on the serialization format, such as Protobuf.
/// - Validate the parsed resource against the expected schema.
///
/// It should return `Err` if parsing fails or validation fails.
/// The error message will be included in the NACK's `error_detail`.
///
/// # Example
///
/// ```ignore
/// impl Resource for Listener {
///     const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.listener.v3.Listener");
///
///     fn decode(bytes: Bytes) -> Result<Self> {
///         let proto = ListenerProto::decode(bytes)?;
///         // Validate fields...
///         Ok(Self { name: proto.name, /* ... */ })
///     }
///
///     fn name(&self) -> &str {
///         &self.name
///     }
/// }
/// ```
pub trait Resource: Sized + Send + Sync + 'static {
    /// The xDS type URL for this resource type.
    const TYPE_URL: TypeUrl;

    /// Decode and validate a resource from its serialized bytes.
    ///
    /// Returns `Err` if parsing fails or validation fails.
    fn decode(bytes: Bytes) -> Result<Self>;

    /// Returns the resource name.
    ///
    /// The resource name combined with the type URL uniquely identifies a resource.
    fn name(&self) -> &str;
}

/// A decoded resource with type-erased value.
///
/// Created by the decoder function when a response is received from the xDS server.
/// The worker stores and dispatches these to watchers, which downcast to the concrete type.
///
/// This type is cheaply cloneable (via `Arc`) to support multiple watchers
/// for the same resource.
#[derive(Debug, Clone)]
pub struct DecodedResource {
    type_url: &'static str,
    name: String,
    value: Arc<dyn Any + Send + Sync>,
}

impl DecodedResource {
    /// Create a new decoded resource from a concrete resource type.
    pub fn new<T: Resource>(resource: T) -> Self {
        Self {
            type_url: T::TYPE_URL.as_str(),
            name: resource.name().to_string(),
            value: Arc::new(resource),
        }
    }

    /// Returns the type URL of the resource.
    pub fn type_url(&self) -> &'static str {
        self.type_url
    }

    /// Returns the name of the resource.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Downcast to a concrete type wrapped in `Arc`.
    ///
    /// Returns `None` if the type does not match.
    ///
    /// This returns `Arc<T>` because `DecodedResource` may be cloned and shared
    /// across multiple watchers. Each watcher receives a reference to the same
    /// underlying resource data.
    ///
    /// This method clones the internal `Arc` (cheap refcount increment) and
    /// attempts to downcast it to the concrete type.
    pub fn downcast<T: Resource>(&self) -> Option<Arc<T>> {
        Arc::clone(&self.value).downcast().ok()
    }

    /// Borrow the decoded resource and downcast to a concrete type reference.
    ///
    /// Returns `None` if the type does not match.
    pub fn downcast_ref<T: Resource>(&self) -> Option<&T> {
        self.value.downcast_ref()
    }
}

/// Type-erased decoder function.
///
/// Created by `XdsClient::watch()` capturing the resource type `T`.
/// The worker stores this per type_url and uses it to decode incoming resources.
pub type DecoderFn = Box<dyn Fn(Bytes) -> Result<DecodedResource> + Send + Sync>;
