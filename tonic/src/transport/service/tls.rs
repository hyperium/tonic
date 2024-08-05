use std::{fmt, io::Cursor};

use tokio_rustls::rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    RootCertStore,
};

use crate::transport::Identity;

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

pub(crate) fn load_identity(
    identity: Identity,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError> {
    let cert = rustls_pemfile::certs(&mut Cursor::new(identity.cert))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TlsError::CertificateParseError)?;

    let Ok(Some(key)) = rustls_pemfile::private_key(&mut Cursor::new(identity.key)) else {
        return Err(TlsError::PrivateKeyParseError);
    };

    Ok((cert, key))
}

pub(crate) fn add_certs_from_pem(
    mut certs: &mut dyn std::io::BufRead,
    roots: &mut RootCertStore,
) -> Result<(), crate::Error> {
    for cert in rustls_pemfile::certs(&mut certs).collect::<Result<Vec<_>, _>>()? {
        roots
            .add(cert)
            .map_err(|_| TlsError::CertificateParseError)?;
    }

    Ok(())
}
