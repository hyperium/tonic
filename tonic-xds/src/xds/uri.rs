use thiserror::Error;

/// Error type for parsing xDS URIs.
#[derive(Debug, Error)]
pub enum XdsUriError {
    /// The URI scheme is not "xds".
    #[error("URI scheme must be 'xds'")]
    InvalidScheme,
    /// The URI could not be parsed.
    #[error("invalid URI: {0}")]
    InvalidUri(#[from] http::uri::InvalidUri),
}

/// An xDS target URI (e.g., `xds:///my-service`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdsUri {
    /// The domain name extracted from the URI.
    pub domain: String,
    /// Optional authority (the part between `xds://` and `/`).
    pub authority: Option<String>,
}

const XDS_SCHEME: &str = "xds";

impl XdsUri {
    /// Parses an xDS URI from a string. Currently only supports URIs with the `xds` scheme.
    pub fn parse(uri: &str) -> Result<Self, XdsUriError> {
        let uri = uri.parse::<http::Uri>()?;
        
        if uri.scheme_str() != Some(XDS_SCHEME) {
            return Err(XdsUriError::InvalidScheme);
        }
        
        let domain = uri.path().trim_start_matches('/').to_string();
        let authority = uri.authority().map(|a| a.to_string());
        
        Ok(Self { domain, authority })
    }
}