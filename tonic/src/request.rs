use crate::metadata::MetadataMap;
use futures_core::Stream;

/// A marker trait that should be implemented for all input messages.
pub trait RequestMessage {}

#[doc(hidden)]
pub trait IntoRequest {
    type Message: RequestMessage;

    fn into_request(self) -> Request<Self::Message>;
}

#[doc(hidden)]
pub trait IntoStreamingRequest {
    type Stream: Stream<Item = Self::Message> + Send + 'static;
    type Message: RequestMessage;

    fn into_streaming_request(self) -> Request<Self::Stream>;
}

/// A gRPC request and metadata from an RPC call.
#[derive(Debug)]
pub struct Request<T> {
    metadata: MetadataMap,
    message: T,
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

impl<T: RequestMessage> IntoRequest for T {
    type Message = Self;

    fn into_request(self) -> Request<T> {
        Request::new(self)
    }
}

impl<T: RequestMessage> IntoRequest for Request<T> {
    type Message = T;

    fn into_request(self) -> Request<T> {
        self
    }
}

impl<T> IntoStreamingRequest for Request<T>
where
    T: Stream + Send + 'static,
    T::Item: RequestMessage,
{
    type Stream = T;
    type Message = T::Item;

    fn into_streaming_request(self) -> Self {
        self
    }
}

impl<T> IntoStreamingRequest for T
where
    T: Stream + Send + 'static,
    T::Item: RequestMessage,
{
    type Stream = T;
    type Message = T::Item;

    fn into_streaming_request(self) -> Request<Self> {
        Request::new(self)
    }
}
