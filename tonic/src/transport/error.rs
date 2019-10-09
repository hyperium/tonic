use std::{error, fmt};

/// Error's that originate from the client or server;
pub struct Error {
    kind: ErrorKind,
    source: Option<crate::Error>,
}

impl Error {
    pub(crate) fn from_source(kind: ErrorKind, source: crate::Error) -> Self {
        Self {
            kind,
            source: Some(source),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ErrorKind {
    Client,
    Server,
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_tuple("Error");
        f.field(&self.kind);
        if let Some(source) = &self.source {
            f.field(source);
        }
        f.finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
