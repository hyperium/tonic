use base64::Engine as _;

/// Represents a X509 certificate.
#[derive(Debug, Clone)]
pub struct Certificate {
    pub(crate) pem: Vec<u8>,
}

/// Represents a private key and X509 certificate.
#[derive(Debug, Clone)]
pub struct Identity {
    pub(crate) cert: Certificate,
    pub(crate) key: Vec<u8>,
}

impl Certificate {
    /// Parse a PEM encoded X509 Certificate.
    ///
    /// The provided PEM should include at least one PEM encoded certificate.
    pub fn from_pem(pem: impl AsRef<[u8]>) -> Self {
        let pem = pem.as_ref().into();
        Self { pem }
    }

    /// Parse a DER encoded X509 Certificate.
    ///
    /// The provided DER should include exactly one DER encoded certificate.
    pub fn from_der(der: impl AsRef<[u8]>) -> Self {
        let der = der.as_ref();
        let pem = der2pem(der);
        Self { pem: pem.into_bytes() }
    }

    /// Get a immutable reference to underlying certificate
    pub fn get_ref(&self) -> &[u8] {
        self.pem.as_slice()
    }

    /// Get a mutable reference to underlying certificate
    pub fn get_mut(&mut self) -> &mut [u8] {
        self.pem.as_mut()
    }

    /// Consumes `self`, returning the underlying certificate
    pub fn into_inner(self) -> Vec<u8> {
        self.pem
    }
}

impl AsRef<[u8]> for Certificate {
    fn as_ref(&self) -> &[u8] {
        self.pem.as_ref()
    }
}

impl AsMut<[u8]> for Certificate {
    fn as_mut(&mut self) -> &mut [u8] {
        self.pem.as_mut()
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

    /// Parse a DER encoded certificate and private key.
    /// 
    /// The provided cert must contain exactly one DER encoded certificate.
    pub fn from_der(cert: impl AsRef<[u8]>, key: impl AsRef<[u8]>) -> Self {
        let cert = Certificate::from_der(cert);
        let key = key.as_ref().into();
        Self { cert, key }
    }
}

fn der2pem(der: &[u8]) -> String {
    const RFC7468_HEADER: &str = "-----BEGIN CERTIFICATE-----";
    const RFC7468_FOOTER: &str = "-----END CERTIFICATE-----";
    let pem = crate::util::base64::STANDARD_NO_PAD.encode(der);
    format!("{RFC7468_HEADER}\n{pem}\n{RFC7468_FOOTER}\n")
}

