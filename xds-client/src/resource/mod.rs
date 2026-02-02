//! Provides abstraction for xDS resources.

use crate::error::Error;
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

/// Result of decoding a resource.
///
/// This enum represents the three possible outcomes of decoding, following
/// the pattern used in grpc-go's xDS client (gRFC A46/A88):
///
/// - [`Success`](DecodeResult::Success): Resource decoded and validated successfully.
/// - [`ResourceError`](DecodeResult::ResourceError): Decoding failed but the resource
///   name was identified. The error can be routed to the specific watcher.
/// - [`TopLevelError`](DecodeResult::TopLevelError): Decoding failed and the resource
///   name could not be identified. No specific watcher can be notified.
#[derive(Debug)]
pub enum DecodeResult<T> {
    /// Resource decoded and validated successfully.
    Success {
        /// The resource name.
        name: String,
        /// The decoded resource.
        resource: T,
    },

    /// Error decoding a resource whose name could be identified.
    ///
    /// This typically occurs when deserialization succeeded (allowing the name
    /// to be extracted) but subsequent validation failed.
    /// The error will be reported to watchers interested in this specific resource.
    ResourceError {
        /// The resource name that was extracted before the error occurred.
        name: String,
        /// The error that occurred during validation.
        error: Error,
    },

    /// Error decoding a resource whose name could not be identified.
    ///
    /// This typically occurs when deserialization fails early, before the resource
    /// name can be extracted. Since we don't know which resource this was meant to be,
    /// no specific watcher can be notified. The error is included in the NACK
    /// message sent back to the server.
    TopLevelError(Error),
}

/// Trait for xDS resources.
///
/// # Two-Phase Decoding
///
/// Resource decoding is split into two phases to support per-resource error
/// reporting (gRFC A46/A88):
///
/// 1. **Deserialization**: Parse bytes into the [`Message`](Self::Message) type.
///    If this fails, no resource name is available ([`DecodeResult::TopLevelError`]).
///
/// 2. **Validation**: Transform the message into the final resource type.
///    If this fails, the resource name is known ([`DecodeResult::ResourceError`]).
///
/// The provided [`decode`](Self::decode) method orchestrates these phases and
/// returns the appropriate [`DecodeResult`].
///
/// # Resource Deletion in State of the World (SotW)
///
/// In SotW xDS, the server sends all resources in each response. The client must
/// determine whether a previously-seen resource that's absent from a new response
/// has been deleted or is simply not included.
///
/// Per gRFC A53, the behavior depends on the resource type:
///
/// - **`ALL_RESOURCES_REQUIRED_IN_SOTW = true`** (default): The server always
///   sends all resources of this type in each response. If a subscribed
///   resource is missing, it's treated as deleted. Watchers receive `ResourceDoesNotExist`.
///   Examples: Listener (LDS), Cluster (CDS).
///
/// - **`ALL_RESOURCES_REQUIRED_IN_SOTW = false`**: The resource type allows partial
///   responses. Missing resources are not treated as deleted; the client continues
///   using the cached version. Examples: RouteConfiguration (RDS), ClusterLoadAssignment (EDS).
///
/// # Example
///
/// ```ignore
/// impl Resource for Listener {
///     type Message = ListenerProto;
///
///     const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.listener.v3.Listener");
///
///     fn deserialize(bytes: Bytes) -> Result<Self::Message, Error> {
///         ListenerProto::decode(bytes).map_err(Into::into)
///     }
///
///     fn name(message: &Self::Message) -> &str {
///         &message.name
///     }
///
///     fn validate(message: Self::Message) -> Result<Self, Error> {
///         // Validation and transformation logic...
///         Ok(Self { name: message.name, /* ... */ })
///     }
/// }
/// ```
pub trait Resource: Sized + Send + Sync + 'static {
    /// The deserialized message type (e.g., protobuf-generated struct).
    type Message;

    /// The xDS type URL for this resource type.
    const TYPE_URL: TypeUrl;

    /// Whether all subscribed resources must be present in each SotW response.
    ///
    /// When `true` (default), if a previously-received resource is absent from a new
    /// response, it is treated as deleted. Watchers are notified with `ResourceDoesNotExist`.
    ///
    /// When `false`, missing resources are not treated as deleted. The client continues
    /// using the cached version until explicitly removed or updated.
    ///
    /// Per gRFC A53:
    /// - LDS (Listener) and CDS (Cluster): `true`
    /// - RDS (RouteConfiguration) and EDS (ClusterLoadAssignment): `false`
    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = true;

    /// Deserialize bytes into the message type.
    ///
    /// This is the first phase of decoding. If this fails, no resource name
    /// is available and the error becomes a [`DecodeResult::TopLevelError`].
    fn deserialize(bytes: Bytes) -> Result<Self::Message, Error>;

    /// Extract the resource name from the deserialized message.
    fn name(message: &Self::Message) -> &str;

    /// Validate and transform the message into the final resource type.
    ///
    /// This is the second phase of decoding. If this fails, the resource name
    /// is known (from [`name`](Self::name)) and the error becomes
    /// a [`DecodeResult::ResourceError`].
    fn validate(message: Self::Message) -> Result<Self, Error>;
}

/// Decode and validate a resource from its serialized bytes.
///
/// This function orchestrates the two-phase decoding process:
/// 1. Deserialize bytes into [`Resource::Message`]
/// 2. Validate and transform into `T`
///
/// Returns the appropriate [`DecodeResult`] based on where (if anywhere) the error occurred.
pub(crate) fn decode<T: Resource>(bytes: Bytes) -> DecodeResult<T> {
    let message = match T::deserialize(bytes) {
        Ok(m) => m,
        Err(e) => return DecodeResult::TopLevelError(e),
    };

    let name = T::name(&message).to_string();

    match T::validate(message) {
        Ok(resource) => DecodeResult::Success { name, resource },
        Err(e) => DecodeResult::ResourceError { name, error: e },
    }
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
    pub fn new<T: Resource>(name: String, resource: T) -> Self {
        Self {
            type_url: T::TYPE_URL.as_str(),
            name,
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
///
/// Returns a [`DecodeResult`] indicating success or the type of failure.
pub type DecoderFn = Box<dyn Fn(Bytes) -> DecodeResult<DecodedResource> + Send + Sync>;
