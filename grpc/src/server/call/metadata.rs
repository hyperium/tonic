use http::HeaderMap;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Metadata {
    pub(crate) inner: HeaderMap,
}

const GRPC_ENCODING: &str = "grpc-encoding";
const GRPC_ACCEPT_ENCODING: &str = "grpc-accept-encoding";
const PATH: &str = "path";

impl Metadata {
    pub fn new(inner: HeaderMap) -> Self {
        Self { inner }
    }

    /// Returns the method name.
    pub fn method_name(&self) -> Option<&str> {
        self.inner.get(PATH).and_then(|v| v.to_str().ok())
    }

    /// Returns the `grpc-encoding` header value.
    pub fn encoding(&self) -> Option<&str> {
        self.inner
            .get(GRPC_ENCODING)
            .and_then(|v| v.to_str().ok())
    }

    /// Returns an iterator over the `grpc-accept-encoding` values.
    /// Handles multiple headers and comma-separated values.
    pub fn accept_encodings(&self) -> impl Iterator<Item = &str> + '_ {
        self.inner
            .get_all(GRPC_ACCEPT_ENCODING)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|s| s.split(','))
            .map(|s| s.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn test_method_name() {
        let mut map = HeaderMap::new();
        map.insert("path", HeaderValue::from_static("/Service/Method"));
        let metadata = Metadata::new(map);

        assert_eq!(metadata.method_name(), Some("/Service/Method"));

        let empty = Metadata::default();
        assert_eq!(empty.method_name(), None);
    }

    #[test]
    fn test_encoding() {
        let mut map = HeaderMap::new();
        map.insert("grpc-encoding", HeaderValue::from_static("gzip"));
        let metadata = Metadata::new(map);

        assert_eq!(metadata.encoding(), Some("gzip"));

        let empty = Metadata::default();
        assert_eq!(empty.encoding(), None);
    }

    #[test]
    fn test_accept_encodings() {
        let mut map = HeaderMap::new();
        map.insert(
            "grpc-accept-encoding",
            HeaderValue::from_static("gzip,identity"),
        );
        let metadata = Metadata::new(map);

        let encodings: Vec<_> = metadata.accept_encodings().collect();
        assert_eq!(encodings, vec!["gzip", "identity"]);

        // Test multiple headers
        let mut map = HeaderMap::new();
        map.append("grpc-accept-encoding", HeaderValue::from_static("gzip"));
        map.append(
            "grpc-accept-encoding",
            HeaderValue::from_static("deflate, br"),
        );
        let metadata = Metadata::new(map);

        let encodings: Vec<_> = metadata.accept_encodings().collect();
        assert_eq!(encodings, vec!["gzip", "deflate", "br"]);
    }
}
