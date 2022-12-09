/// The `Status` type defines a logical error model that is suitable for
/// different programming environments, including REST APIs and RPC APIs. It is
/// used by \[gRPC\](<https://github.com/grpc>). Each `Status` message contains
/// three pieces of data: error code, error message, and error details.
///
/// You can find out more about this error model and how to work with it in the
/// [API Design Guide](<https://cloud.google.com/apis/design/errors>).
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Status {
    /// The status code, which should be an enum value of \\[google.rpc.Code\]\[google.rpc.Code\\].
    #[prost(int32, tag = "1")]
    pub code: i32,
    /// A developer-facing error message, which should be in English. Any
    /// user-facing error message should be localized and sent in the
    /// \\[google.rpc.Status.details\]\[google.rpc.Status.details\\] field, or localized by the client.
    #[prost(string, tag = "2")]
    pub message: ::prost::alloc::string::String,
    /// A list of messages that carry the error details.  There is a common set of
    /// message types for APIs to use.
    #[prost(message, repeated, tag = "3")]
    pub details: ::prost::alloc::vec::Vec<::prost_types::Any>,
}
/// Describes when the clients can retry a failed request. Clients could ignore
/// the recommendation here or retry when this information is missing from error
/// responses.
///
/// It's always recommended that clients should use exponential backoff when
/// retrying.
///
/// Clients should wait until `retry_delay` amount of time has passed since
/// receiving the error response before retrying.  If retrying requests also
/// fail, clients should use an exponential backoff scheme to gradually increase
/// the delay between retries based on `retry_delay`, until either a maximum
/// number of retries have been reached or a maximum retry delay cap has been
/// reached.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RetryInfo {
    /// Clients should wait at least this long between retrying the same request.
    #[prost(message, optional, tag = "1")]
    pub retry_delay: ::core::option::Option<::prost_types::Duration>,
}
/// Describes additional debugging info.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DebugInfo {
    /// The stack trace entries indicating where the error occurred.
    #[prost(string, repeated, tag = "1")]
    pub stack_entries: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    /// Additional debugging information provided by the server.
    #[prost(string, tag = "2")]
    pub detail: ::prost::alloc::string::String,
}
/// Describes how a quota check failed.
///
/// For example if a daily limit was exceeded for the calling project,
/// a service could respond with a QuotaFailure detail containing the project
/// id and the description of the quota limit that was exceeded.  If the
/// calling project hasn't enabled the service in the developer console, then
/// a service could respond with the project id and set `service_disabled`
/// to true.
///
/// Also see RetryInfo and Help types for other details about handling a
/// quota failure.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QuotaFailure {
    /// Describes all quota violations.
    #[prost(message, repeated, tag = "1")]
    pub violations: ::prost::alloc::vec::Vec<quota_failure::Violation>,
}
/// Nested message and enum types in `QuotaFailure`.
pub mod quota_failure {
    /// A message type used to describe a single quota violation.  For example, a
    /// daily quota or a custom quota that was exceeded.
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Violation {
        /// The subject on which the quota check failed.
        /// For example, "clientip:<ip address of client>" or "project:<Google
        /// developer project id>".
        #[prost(string, tag = "1")]
        pub subject: ::prost::alloc::string::String,
        /// A description of how the quota check failed. Clients can use this
        /// description to find more about the quota configuration in the service's
        /// public documentation, or find the relevant quota limit to adjust through
        /// developer console.
        ///
        /// For example: "Service disabled" or "Daily Limit for read operations
        /// exceeded".
        #[prost(string, tag = "2")]
        pub description: ::prost::alloc::string::String,
    }
}
/// Describes the cause of the error with structured details.
///
/// Example of an error when contacting the "pubsub.googleapis.com" API when it
/// is not enabled:
///
/// ```text,json
///     { "reason": "API_DISABLED"
///       "domain": "googleapis.com"
///       "metadata": {
///         "resource": "projects/123",
///         "service": "pubsub.googleapis.com"
///       }
///     }
/// ```
///
/// This response indicates that the pubsub.googleapis.com API is not enabled.
///
/// Example of an error that is returned when attempting to create a Spanner
/// instance in a region that is out of stock:
///
/// ```text,json
///     { "reason": "STOCKOUT"
///       "domain": "spanner.googleapis.com",
///       "metadata": {
///         "availableRegions": "us-central1,us-east2"
///       }
///     }
/// ```
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ErrorInfo {
    /// The reason of the error. This is a constant value that identifies the
    /// proximate cause of the error. Error reasons are unique within a particular
    /// domain of errors. This should be at most 63 characters and match
    /// /\\[A-Z0-9\_\\]+/.
    #[prost(string, tag = "1")]
    pub reason: ::prost::alloc::string::String,
    /// The logical grouping to which the "reason" belongs. The error domain
    /// is typically the registered service name of the tool or product that
    /// generates the error. Example: "pubsub.googleapis.com". If the error is
    /// generated by some common infrastructure, the error domain must be a
    /// globally unique value that identifies the infrastructure. For Google API
    /// infrastructure, the error domain is "googleapis.com".
    #[prost(string, tag = "2")]
    pub domain: ::prost::alloc::string::String,
    /// Additional structured details about this error.
    ///
    /// Keys should match /\\[a-zA-Z0-9-\_\\]/ and be limited to 64 characters in
    /// length. When identifying the current value of an exceeded limit, the units
    /// should be contained in the key, not the value.  For example, rather than
    /// {"instanceLimit": "100/request"}, should be returned as,
    /// {"instanceLimitPerRequest": "100"}, if the client exceeds the number of
    /// instances that can be created in a single (batch) request.
    #[prost(map = "string, string", tag = "3")]
    pub metadata: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
}
/// Describes what preconditions have failed.
///
/// For example, if an RPC failed because it required the Terms of Service to be
/// acknowledged, it could list the terms of service violation in the
/// PreconditionFailure message.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PreconditionFailure {
    /// Describes all precondition violations.
    #[prost(message, repeated, tag = "1")]
    pub violations: ::prost::alloc::vec::Vec<precondition_failure::Violation>,
}
/// Nested message and enum types in `PreconditionFailure`.
pub mod precondition_failure {
    /// A message type used to describe a single precondition failure.
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Violation {
        /// The type of PreconditionFailure. We recommend using a service-specific
        /// enum type to define the supported precondition violation subjects. For
        /// example, "TOS" for "Terms of Service violation".
        #[prost(string, tag = "1")]
        pub r#type: ::prost::alloc::string::String,
        /// The subject, relative to the type, that failed.
        /// For example, "google.com/cloud" relative to the "TOS" type would indicate
        /// which terms of service is being referenced.
        #[prost(string, tag = "2")]
        pub subject: ::prost::alloc::string::String,
        /// A description of how the precondition failed. Developers can use this
        /// description to understand how to fix the failure.
        ///
        /// For example: "Terms of service not accepted".
        #[prost(string, tag = "3")]
        pub description: ::prost::alloc::string::String,
    }
}
/// Describes violations in a client request. This error type focuses on the
/// syntactic aspects of the request.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BadRequest {
    /// Describes all violations in a client request.
    #[prost(message, repeated, tag = "1")]
    pub field_violations: ::prost::alloc::vec::Vec<bad_request::FieldViolation>,
}
/// Nested message and enum types in `BadRequest`.
pub mod bad_request {
    /// A message type used to describe a single bad request field.
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct FieldViolation {
        /// A path leading to a field in the request body. The value will be a
        /// sequence of dot-separated identifiers that identify a protocol buffer
        /// field. E.g., "field_violations.field" would identify this field.
        #[prost(string, tag = "1")]
        pub field: ::prost::alloc::string::String,
        /// A description of why the request element is bad.
        #[prost(string, tag = "2")]
        pub description: ::prost::alloc::string::String,
    }
}
/// Contains metadata about the request that clients can attach when filing a bug
/// or providing other forms of feedback.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RequestInfo {
    /// An opaque string that should only be interpreted by the service generating
    /// it. For example, it can be used to identify requests in the service's logs.
    #[prost(string, tag = "1")]
    pub request_id: ::prost::alloc::string::String,
    /// Any data that was used to serve this request. For example, an encrypted
    /// stack trace that can be sent back to the service provider for debugging.
    #[prost(string, tag = "2")]
    pub serving_data: ::prost::alloc::string::String,
}
/// Describes the resource that is being accessed.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ResourceInfo {
    /// A name for the type of resource being accessed, e.g. "sql table",
    /// "cloud storage bucket", "file", "Google calendar"; or the type URL
    /// of the resource: e.g. "type.googleapis.com/google.pubsub.v1.Topic".
    #[prost(string, tag = "1")]
    pub resource_type: ::prost::alloc::string::String,
    /// The name of the resource being accessed.  For example, a shared calendar
    /// name: "example.com_4fghdhgsrgh@group.calendar.google.com", if the current
    /// error is \\[google.rpc.Code.PERMISSION_DENIED\]\[google.rpc.Code.PERMISSION_DENIED\\].
    #[prost(string, tag = "2")]
    pub resource_name: ::prost::alloc::string::String,
    /// The owner of the resource (optional).
    /// For example, "user:<owner email>" or "project:<Google developer project
    /// id>".
    #[prost(string, tag = "3")]
    pub owner: ::prost::alloc::string::String,
    /// Describes what error is encountered when accessing this resource.
    /// For example, updating a cloud project may require the `writer` permission
    /// on the developer console project.
    #[prost(string, tag = "4")]
    pub description: ::prost::alloc::string::String,
}
/// Provides links to documentation or for performing an out of band action.
///
/// For example, if a quota check failed with an error indicating the calling
/// project hasn't enabled the accessed service, this can contain a URL pointing
/// directly to the right place in the developer console to flip the bit.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Help {
    /// URL(s) pointing to additional information on handling the current error.
    #[prost(message, repeated, tag = "1")]
    pub links: ::prost::alloc::vec::Vec<help::Link>,
}
/// Nested message and enum types in `Help`.
pub mod help {
    /// Describes a URL link.
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Link {
        /// Describes what the link offers.
        #[prost(string, tag = "1")]
        pub description: ::prost::alloc::string::String,
        /// The URL of the link.
        #[prost(string, tag = "2")]
        pub url: ::prost::alloc::string::String,
    }
}
/// Provides a localized error message that is safe to return to the user
/// which can be attached to an RPC error.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LocalizedMessage {
    /// The locale used following the specification defined at
    /// <http://www.rfc-editor.org/rfc/bcp/bcp47.txt.>
    /// Examples are: "en-US", "fr-CH", "es-MX"
    #[prost(string, tag = "1")]
    pub locale: ::prost::alloc::string::String,
    /// The localized error message in the above locale.
    #[prost(string, tag = "2")]
    pub message: ::prost::alloc::string::String,
}
