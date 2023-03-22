use std::{error::Error as StdError, fmt};

type Source = Box<dyn StdError + Send + Sync + 'static>;

/// Error's that originate from the client or server;
pub struct Error {
    inner: ErrorImpl,
}

struct ErrorImpl {
    kind: Kind,
    source: Option<Source>,
}

#[derive(Debug)]
pub(crate) enum Kind {
    Transport,
    InvalidUri,
    InvalidUserAgent,
}

impl Error {
    pub(crate) fn new(kind: Kind) -> Self {
        Self {
            inner: ErrorImpl { kind, source: None },
        }
    }

    pub(crate) fn with(mut self, source: impl Into<Source>) -> Self {
        self.inner.source = Some(source.into());
        self
    }

    pub(crate) fn from_source(source: impl Into<crate::Error>) -> Self {
        Error::new(Kind::Transport).with(source)
    }

    pub(crate) fn new_invalid_uri() -> Self {
        Error::new(Kind::InvalidUri)
    }

    pub(crate) fn new_invalid_user_agent() -> Self {
        Error::new(Kind::InvalidUserAgent)
    }

    fn description(&self) -> &str {
        match &self.inner.kind {
            Kind::Transport => "transport error",
            Kind::InvalidUri => "invalid URI",
            Kind::InvalidUserAgent => "user agent is not a valid header value",
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
