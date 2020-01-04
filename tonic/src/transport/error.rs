use std::{error, fmt};

/// Error's that originate from the client or server;
#[derive(Debug)]
pub struct Error(crate::Error);

impl Error {
    pub(crate) fn from_source(source: impl Into<crate::Error>) -> Self {
        Self(source.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        self.0.source()
    }
}
