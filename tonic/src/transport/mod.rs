mod channel;
mod endpoint;
mod service;
mod tls;

pub use self::channel::Channel;
pub use self::endpoint::Endpoint;

use std::{error, fmt};

pub struct Error {
    kind: ErrorKind,
    source: Option<crate::Error>,
}

#[derive(Debug)]
pub(crate) enum ErrorKind {
    Client,
    // Server,
}

impl From<ErrorKind> for Error {
    fn from(t: ErrorKind) -> Self {
        Self {
            kind: t,
            source: None,
        }
    }
}

impl From<(ErrorKind, crate::Error)> for Error {
    fn from(t: (ErrorKind, crate::Error)) -> Self {
        Self {
            kind: t.0,
            source: Some(t.1),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut f = f.debug_tuple("Error");
        f.field(&self.kind);
        if let Some(source) = &self.source {
            f.field(source);
        }
        f.finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(source) = &self.source {
            write!(f, "{}: {}", self.kind, source)
        } else {
            write!(f, "{}", self.kind)
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| &**e as &(dyn error::Error + 'static))
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
