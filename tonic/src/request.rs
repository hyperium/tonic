use crate::metadata::MetadataMap;
use futures_core::Stream;

/// A gRPC request and metadata from an RPC call.
#[derive(Debug)]
pub struct Request<T> {
    metadata: MetadataMap,
    message: T,
}

/// Trait implemented by RPC request types.
///
/// Types implementing this trait can be used as arguments to client RPC
/// methods without explicitly wrapping them into `tonic::Request`s. The purpose
/// is to make client calls slightly more convenient to write.
///
/// Tonic's code generation and blanket implementations handle this for you,
/// so it is not necessary to implement this trait directly.
///
/// # Example
///
/// Given the following gRPC method definition:
/// ```proto
/// rpc GetFeature(Point) returns (Feature) {}
/// ```
///
/// we can call `get_feature` in two equivalent ways:
/// ```rust
/// # pub struct Point {}
/// # pub struct Client {}
/// # impl Client {
/// #   fn get_feature(&self, r: impl tonic::IntoRequest<Point>) {}
/// # }
/// # let client = Client {};
/// use tonic::Request;
///
/// client.get_feature(Point {});
/// client.get_feature(Request::new(Point {}));
/// ```
pub trait IntoRequest<T>: sealed::Sealed {
    /// Wrap the input message `T` in a `tonic::Request`
    fn into_request(self) -> Request<T>;
}

/// Trait implemented by RPC streaming request types.
///
/// Types implementing this trait can be used as arguments to client streaming
/// RPC methods without explicitly wrapping them into `tonic::Request`s. The
/// purpose is to make client calls slightly more convenient to write.
///
/// Tonic's code generation and blanket implementations handle this for you,
/// so it is not necessary to implement this trait directly.
///
/// # Example
///
/// Given the following gRPC service method definition:
/// ```proto
/// rpc RecordRoute(stream Point) returns (RouteSummary) {}
/// ```
/// we can call `record_route` in two equivalent ways:
///
/// ```rust
/// # #[derive(Clone)]
/// # pub struct Point {};
/// # pub struct Client {};
/// # impl Client {
/// #   fn record_route(&self, r: impl tonic::IntoStreamingRequest<Message = Point>) {}
/// # }
/// # let client = Client {};
/// use tonic::Request;
/// use futures_util::stream;
///
/// let messages = vec![Point {}, Point {}];
///
/// client.record_route(Request::new(stream::iter(messages.clone())));
/// client.record_route(stream::iter(messages));
/// ```
pub trait IntoStreamingRequest: sealed::Sealed {
    /// The RPC request stream type
    type Stream: Stream<Item = Self::Message> + Send + 'static;

    /// The RPC request type
    type Message;

    /// Wrap the stream of messages in a `tonic::Request`
    fn into_streaming_request(self) -> Request<Self::Stream>;
}

impl<T> Request<T> {
    /// Create a new gRPC request.
    ///
    /// ```rust
    /// # use tonic::Request;
    /// # pub struct HelloRequest {
    /// #   pub name: String,
    /// # }
    /// Request::new(HelloRequest {
    ///    name: "Bob".into(),
    /// });
    /// ```
    pub fn new(message: T) -> Self {
        Request {
            metadata: MetadataMap::new(),
            message,
        }
    }

    /// Get a reference to the message
    pub fn get_ref(&self) -> &T {
        &self.message
    }

    /// Get a mutable reference to the message
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.message
    }

    /// Get a reference to the custom request metadata.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Get a mutable reference to the request metadata.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    /// Consumes `self`, returning the message
    pub fn into_inner(self) -> T {
        self.message
    }

    pub(crate) fn from_http_parts(parts: http::request::Parts, message: T) -> Self {
        Request {
            metadata: MetadataMap::from_headers(parts.headers),
            message,
        }
    }

    /// Convert an HTTP request to a gRPC request
    pub fn from_http(http: http::Request<T>) -> Self {
        let (parts, message) = http.into_parts();
        Request::from_http_parts(parts, message)
    }

    pub(crate) fn into_http(self, uri: http::Uri) -> http::Request<T> {
        let mut request = http::Request::new(self.message);

        *request.version_mut() = http::Version::HTTP_2;
        *request.method_mut() = http::Method::POST;
        *request.uri_mut() = uri;
        *request.headers_mut() = self.metadata.into_headers();

        request
    }

    #[doc(hidden)]
    pub fn map<F, U>(self, f: F) -> Request<U>
    where
        F: FnOnce(T) -> U,
    {
        let message = f(self.message);

        Request {
            metadata: self.metadata,
            message,
        }
    }
}

impl<T> IntoRequest<T> for T {
    fn into_request(self) -> Request<Self> {
        Request::new(self)
    }
}

impl<T> IntoRequest<T> for Request<T> {
    fn into_request(self) -> Request<T> {
        self
    }
}

impl<T> IntoStreamingRequest for T
where
    T: Stream + Send + 'static,
{
    type Stream = T;
    type Message = T::Item;

    fn into_streaming_request(self) -> Request<Self> {
        Request::new(self)
    }
}

impl<T> IntoStreamingRequest for Request<T>
where
    T: Stream + Send + 'static,
{
    type Stream = T;
    type Message = T::Item;

    fn into_streaming_request(self) -> Self {
        self
    }
}

impl<T> sealed::Sealed for T {}

mod sealed {
    pub trait Sealed {}
}
