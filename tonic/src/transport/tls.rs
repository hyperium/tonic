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

    /// Parse a DER encoded X509 Certificate.
    ///
    /// The provided DER should include at least one PEM encoded certificate.
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

    /// Returns whether this is a DER encoded certificate.
    pub fn is_der(&self) -> bool {
        matches!(self.kind, CertKind::Der(_))
    }

    /// Returns whether this is a PEM encoded certificate.
    pub fn is_pem(&self) -> bool {
        matches!(self.kind, CertKind::Pem(_))
    }

    /// Returns the reference to DER encoded certificate.
    /// Returns `None` When this is not encoded as DER.
    pub fn der(&self) -> Option<&[u8]> {
        match &self.kind {
            CertKind::Der(der) => Some(der),
            _ => None,
        }
    }

    /// Returns the reference to PEM encoded certificate.
    /// Returns `None` When this is not encoded as PEM.
    pub fn pem(&self) -> Option<&[u8]> {
        match &self.kind {
            CertKind::Pem(pem) => Some(pem),
            _ => None,
        }
    }

    /// Turns this value into the DER encoded bytes.
    pub fn into_der(self) -> Result<Vec<u8>, Self> {
        match self.kind {
            CertKind::Der(der) => Ok(der),
            _ => Err(self),
        }
    }

    /// Turns this value into the PEM encoded bytes.
    pub fn into_pem(self) -> Result<Vec<u8>, Self> {
        match self.kind {
            CertKind::Pem(pem) => Ok(pem),
            _ => Err(self),
        }
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
