use thiserror::Error;
use url::Url;

/// Error type for parsing xDS URIs.
#[derive(Debug, Error)]
pub enum XdsUriError {
    /// The URI scheme is not "xds".
    #[error("URI scheme must be 'xds', got '{0}'")]
    InvalidScheme(String),
    /// The URI could not be parsed.
    #[error("invalid URI: {0}")]
    InvalidUri(#[from] url::ParseError),
}

/// An xDS target URI (e.g., `xds:///my-service`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdsUri {
    /// The target service name extracted from the URI.
    pub target: String,
}

const XDS_SCHEME: &str = "xds";

impl XdsUri {
    /// Parses an xDS URI from a string.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URI cannot be parsed as a valid URI ([`XdsUriError::InvalidUri`])
    /// - The URI scheme is not `xds` ([`XdsUriError::InvalidScheme`])
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_xds::XdsUri;
    ///
    /// let uri = XdsUri::parse("xds:///my-service").expect("Failed to parse valid xDS URI");
    /// assert_eq!(uri.target, "my-service");
    ///
    /// let invalid_uri = XdsUri::parse("http:///my-service");
    /// assert!(invalid_uri.is_err());
    /// assert_eq!(invalid_uri.unwrap_err().to_string(), "URI scheme must be 'xds', got 'http'");
    /// ```
    pub fn parse(uri: &str) -> Result<Self, XdsUriError> {
        let uri = Url::parse(uri)?;

        if uri.scheme() != XDS_SCHEME {
            return Err(XdsUriError::InvalidScheme(uri.scheme().to_string()));
        }

        let target = uri.path().trim_start_matches('/').to_string();

        Ok(Self { target })
    }
}
