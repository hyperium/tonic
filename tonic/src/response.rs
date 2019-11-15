use crate::metadata::MetadataMap;

/// A gRPC response and metadata from an RPC call.
#[derive(Debug)]
pub struct Response<T> {
    metadata: MetadataMap,
    message: T,
}

impl<T> Response<T> {
    /// Create a new gRPC response.
    ///
    /// ```rust
    /// # use tonic::Response;
    /// # pub struct HelloReply {
    /// #   pub message: String,
    /// # }
    /// # let name = "";
    /// Response::new(HelloReply {
    ///     message: format!("Hello, {}!", name).into(),
    /// });
    /// ```
    pub fn new(message: T) -> Self {
        Response {
            metadata: MetadataMap::new(),
            message,
        }
    }

    /// Get a immutable reference to `T`.
    pub fn get_ref(&self) -> &T {
        &self.message
    }

    /// Get a mutable reference to the message
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.message
    }

    /// Get a reference to the custom response metadata.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Get a mutable reference to the response metadata.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    /// Consumes `self`, returning the message
    pub fn into_inner(self) -> T {
        self.message
    }

    pub(crate) fn into_parts(self) -> (MetadataMap, T) {
        (self.metadata, self.message)
    }

    pub(crate) fn from_parts(metadata: MetadataMap, message: T) -> Self {
        Self { metadata, message }
    }

    pub(crate) fn from_http(res: http::Response<T>) -> Self {
        let (head, message) = res.into_parts();
        Response {
            metadata: MetadataMap::from_headers(head.headers),
            message,
        }
    }

    pub(crate) fn into_http(self) -> http::Response<T> {
        let mut res = http::Response::new(self.message);

        *res.version_mut() = http::Version::HTTP_2;
        *res.headers_mut() = self.metadata.into_sanitized_headers();

        res
    }

    #[doc(hidden)]
    pub fn map<F, U>(self, f: F) -> Response<U>
    where
        F: FnOnce(T) -> U,
    {
        let message = f(self.message);
        Response {
            metadata: self.metadata,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::MetadataValue;

    #[test]
    fn reserved_headers_are_excluded() {
        let mut r = Response::new(1);

        for header in &MetadataMap::GRPC_RESERVED_HEADERS {
            r.metadata_mut()
                .insert(*header, MetadataValue::from_static("invalid"));
        }

        let http_response = r.into_http();
        assert!(http_response.headers().is_empty());
    }
}
