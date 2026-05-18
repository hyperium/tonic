use std::{error::Error as StdError, fmt};

type Source = Box<dyn StdError + Send + Sync + 'static>;

/// Error's that originate from the client or server;
pub struct Error {
    inner: ErrorImpl,
}

struct ErrorImpl {
    kind: ErrorKind,
    source: Option<Source>,
}

/// A categorical description of a [`transport::Error`](Error).
///
/// Returned by [`Error::kind`], this enum lets callers programmatically
/// distinguish between the different failure modes of the transport layer
/// without inspecting the error's `Display` output or downcasting its
/// [`source`](std::error::Error::source).
///
/// This enum is marked `#[non_exhaustive]`: new variants may be added in
/// the future without a major version bump, so always include a `_ =>`
/// arm when matching on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A generic transport-level failure (I/O, protocol, TLS handshake, etc.).
    /// The underlying cause is available via [`std::error::Error::source`].
    Transport,
    /// The provided value could not be parsed as a valid URI.
    #[cfg(feature = "channel")]
    InvalidUri,
    /// The configured user-agent string is not a valid HTTP header value.
    #[cfg(feature = "channel")]
    InvalidUserAgent,
    /// TLS configuration was applied to an endpoint that uses a Unix domain
    /// socket, which is not supported.
    #[cfg(all(feature = "_tls-any", feature = "channel"))]
    InvalidTlsConfigForUds,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind) -> Self {
        Self {
            inner: ErrorImpl { kind, source: None },
        }
    }

    pub(crate) fn with(mut self, source: impl Into<Source>) -> Self {
        self.inner.source = Some(source.into());
        self
    }

    pub(crate) fn from_source(source: impl Into<crate::BoxError>) -> Self {
        Error::new(ErrorKind::Transport).with(source)
    }

    #[cfg(feature = "channel")]
    pub(crate) fn new_invalid_uri() -> Self {
        Error::new(ErrorKind::InvalidUri)
    }

    #[cfg(feature = "channel")]
    pub(crate) fn new_invalid_user_agent() -> Self {
        Error::new(ErrorKind::InvalidUserAgent)
    }

    /// Returns the [`ErrorKind`] categorizing this error.
    ///
    /// Use this to branch on the failure mode without parsing `Display`
    /// output. Always include a `_` arm — [`ErrorKind`] is `#[non_exhaustive]`.
    ///
    /// ```ignore
    /// use tonic::transport::{Error, ErrorKind};
    ///
    /// fn classify(err: &Error) {
    ///     match err.kind() {
    ///         ErrorKind::Transport => { /* network-level failure */ }
    ///         _ => { /* configuration or other error */ }
    ///     }
    /// }
    /// ```
    pub fn kind(&self) -> ErrorKind {
        self.inner.kind
    }

    fn description(&self) -> &str {
        match &self.inner.kind {
            ErrorKind::Transport => "transport error",
            #[cfg(feature = "channel")]
            ErrorKind::InvalidUri => "invalid URI",
            #[cfg(feature = "channel")]
            ErrorKind::InvalidUserAgent => "user agent is not a valid header value",
            #[cfg(all(feature = "_tls-any", feature = "channel"))]
            ErrorKind::InvalidTlsConfigForUds => "cannot apply TLS config for unix domain socket",
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_tuple("tonic::transport::Error");

        f.field(&self.inner.kind);

        if let Some(source) = &self.inner.source {
            f.field(source);
        }

        f.finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.inner
            .source
            .as_ref()
            .map(|source| &**source as &(dyn StdError + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_source_has_transport_kind() {
        let inner = std::io::Error::other("boom");
        let err = Error::from_source(inner);
        assert_eq!(err.kind(), ErrorKind::Transport);
    }

    #[cfg(feature = "channel")]
    #[test]
    fn invalid_uri_kind() {
        assert_eq!(Error::new_invalid_uri().kind(), ErrorKind::InvalidUri);
    }

    #[cfg(feature = "channel")]
    #[test]
    fn invalid_user_agent_kind() {
        assert_eq!(
            Error::new_invalid_user_agent().kind(),
            ErrorKind::InvalidUserAgent,
        );
    }

    #[test]
    fn error_kind_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<ErrorKind>();
    }
}
