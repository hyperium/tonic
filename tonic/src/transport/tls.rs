use std::{fmt, io::Cursor};

use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Represents a X509 certificate.
#[derive(Debug, Clone)]
pub struct Certificate {
    pub(crate) pem: Vec<u8>,
}

impl Certificate {
    /// Parse a PEM encoded X509 Certificate.
    ///
    /// The provided PEM should include at least one PEM encoded certificate.
    pub fn from_pem(pem: impl AsRef<[u8]>) -> Self {
        let pem = pem.as_ref().into();
        Self { pem }
    }

    pub(crate) fn to_der_certificates(&self) -> Result<Vec<CertificateDer<'static>>, TlsError> {
        rustls_pemfile::certs(&mut Cursor::new(&self.pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TlsError::CertificateParseError)
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

/// Represents a private key and X509 certificate.
#[derive(Debug, Clone)]
pub struct Identity {
    pub(crate) cert: Certificate,
    pub(crate) key: Vec<u8>,
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

pub(crate) fn load_identity(
    identity: Identity,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError> {
    let cert = identity.cert.to_der_certificates()?;

    let Ok(Some(key)) = rustls_pemfile::private_key(&mut Cursor::new(identity.key)) else {
        return Err(TlsError::PrivateKeyParseError);
    };

    Ok((cert, key))
}

#[derive(Debug)]
pub(crate) enum TlsError {
    #[cfg(feature = "channel")]
    H2NotNegotiated,
    CertificateParseError,
    PrivateKeyParseError,
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "channel")]
            TlsError::H2NotNegotiated => write!(f, "HTTP/2 was not negotiated."),
            TlsError::CertificateParseError => write!(f, "Error parsing TLS certificate."),
            TlsError::PrivateKeyParseError => write!(
                f,
                "Error parsing TLS private key - no RSA or PKCS8-encoded keys found."
            ),
        }
    }
}

impl std::error::Error for TlsError {}

/// h2 alpn in plain format for rustls.
pub(crate) const ALPN_H2: &[u8] = b"h2";
