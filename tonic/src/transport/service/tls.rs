use std::{fmt, io::Cursor};

use tokio_rustls::rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    RootCertStore,
};

use crate::transport::{tls::CertKind, Certificate, Identity};

/// h2 alpn in plain format for rustls.
pub(crate) const ALPN_H2: &[u8] = b"h2";

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

fn convert_certificate_to_rustls_certificate_der(
    certificate: Certificate,
) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let cert = match certificate.kind {
        CertKind::Der(der) => vec![der.into()],
        CertKind::Pem(pem) => rustls_pemfile::certs(&mut Cursor::new(pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TlsError::CertificateParseError)?,
    };
    Ok(cert)
}

pub(crate) fn load_identity(
    identity: Identity,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError> {
    let cert = convert_certificate_to_rustls_certificate_der(identity.cert)?;

    let Ok(Some(key)) = rustls_pemfile::private_key(&mut Cursor::new(identity.key)) else {
        return Err(TlsError::PrivateKeyParseError);
    };

    Ok((cert, key))
}

pub(crate) fn add_certificate_to_root_store(
    certificate: Certificate,
    roots: &mut RootCertStore,
) -> Result<(), TlsError> {
    for cert in convert_certificate_to_rustls_certificate_der(certificate)? {
        roots
            .add(cert)
            .map_err(|_| TlsError::CertificateParseError)?;
    }
    Ok(())
}
