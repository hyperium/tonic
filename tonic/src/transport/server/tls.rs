use crate::transport::{
    service::TlsAcceptor,
    tls::{Certificate, Identity},
};
use std::fmt;

/// Configures TLS settings for servers.
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
#[derive(Clone, Default)]
pub struct ServerTlsConfig {
    identity: Option<Identity>,
    client_ca_root: Option<Certificate>,
}

#[cfg(feature = "tls")]
impl fmt::Debug for ServerTlsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerTlsConfig").finish()
    }
}

#[cfg(feature = "tls")]
impl ServerTlsConfig {
    /// Creates a new `ServerTlsConfig`.
    pub fn new() -> Self {
        ServerTlsConfig {
            identity: None,
            client_ca_root: None,
        }
    }

    /// Sets the [`Identity`] of the server.
    pub fn identity(self, identity: Identity) -> Self {
        ServerTlsConfig {
            identity: Some(identity),
            ..self
        }
    }

    /// Sets a certificate against which to validate client TLS certificates.
    pub fn client_ca_root(self, cert: Certificate) -> Self {
        ServerTlsConfig {
            client_ca_root: Some(cert),
            ..self
        }
    }

    pub(crate) fn tls_acceptor(&self) -> Result<TlsAcceptor, crate::Error> {
        TlsAcceptor::new(self.identity.clone().unwrap(), self.client_ca_root.clone())
    }
}
