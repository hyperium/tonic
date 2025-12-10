//! Provides abstraction for xDS resources.

use crate::error::Result;
use bytes::Bytes;

#[cfg(feature = "codegen-prost")]
pub mod prost;

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
///     const TYPE_URL: &'static str = "type.googleapis.com/envoy.config.listener.v3.Listener";
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
pub trait Resource: Send + Sync + Clone + std::fmt::Debug + 'static {
    /// The xDS type URL for this resource type.
    ///
    /// Example: `"type.googleapis.com/envoy.config.listener.v3.Listener"`
    const TYPE_URL: &'static str;

    /// Decode and validate a resource from its serialized bytes.
    ///
    /// Returns `Err` if parsing fails or validation fails.
    fn decode(bytes: Bytes) -> Result<Self>;

    /// Returns the resource name.
    ///
    /// The resource name combined with the type URL uniquely identifies a resource.
    fn name(&self) -> &str;
}
