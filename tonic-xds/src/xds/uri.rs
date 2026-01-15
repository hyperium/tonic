use thiserror::Error;
use url::Url;

/// Error type for parsing xDS URIs.
#[derive(Debug, Error)]
pub enum XdsUriError {
    /// The URI scheme is not "xds".
    #[error("URI scheme must be 'xds'")]
    InvalidScheme,
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
    /// ```
    pub fn parse(uri: &str) -> Result<Self, XdsUriError> {
        let uri = Url::parse(uri)?;

        if uri.scheme() != XDS_SCHEME {
            return Err(XdsUriError::InvalidScheme);
        }

        let target = uri.path().trim_start_matches('/').to_string();

        Ok(Self { target })
    }
}
