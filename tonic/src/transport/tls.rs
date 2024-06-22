/// Represents a X509 certificate.
#[derive(Debug, Clone)]
pub struct Certificate {
    pub(super) kind: CertKind,
}

#[derive(Debug, Clone)]
pub(super) enum CertKind {
    Der(Vec<u8>),
    Pem(Vec<u8>),
}

/// Represents a private key and X509 certificate.
#[derive(Debug, Clone)]
pub struct Identity {
    pub(crate) cert: Certificate,
    pub(crate) key: Vec<u8>,
}

impl Certificate {
    fn new(kind: CertKind) -> Self {
        Self { kind }
    }

    /// Parse a PEM encoded X509 Certificate.
    ///
    /// The provided PEM should include at least one PEM encoded certificate.
    pub fn from_der(der: impl AsRef<[u8]>) -> Self {
        let der = der.as_ref().into();
        Self::new(CertKind::Der(der))
    }

    /// Parse a PEM encoded X509 Certificate.
    ///
    /// The provided PEM should include at least one PEM encoded certificate.
    pub fn from_pem(pem: impl AsRef<[u8]>) -> Self {
        let pem = pem.as_ref().into();
        Self::new(CertKind::Pem(pem))
    }
}

impl Identity {
    /// Parse a PEM encoded certificate and private key.
    ///
    /// The provided cert must contain at least one PEM encoded certificate.
    pub fn from_pem(cert: impl AsRef<[u8]>, key: impl AsRef<[u8]>) -> Self {
        let cert = Certificate::from_pem(cert);
        let key = key.as_ref().into();
        Self { cert, key }
    }
}
