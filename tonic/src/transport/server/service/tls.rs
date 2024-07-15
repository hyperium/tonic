use std::{fmt, io::Cursor, sync::Arc};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{
    rustls::{server::WebPkiClientVerifier, RootCertStore, ServerConfig},
    server::TlsStream,
    TlsAcceptor as RustlsAcceptor,
};

use crate::transport::{
    service::tls::{add_certs_from_pem, load_identity, ALPN_H2},
    Certificate, Identity,
};

#[derive(Clone)]
pub(crate) struct TlsAcceptor {
    inner: Arc<ServerConfig>,
}

impl TlsAcceptor {
    pub(crate) fn new(
        identity: Identity,
        client_ca_root: Option<Certificate>,
        client_auth_optional: bool,
    ) -> Result<Self, crate::Error> {
        let builder = ServerConfig::builder();

        let builder = match client_ca_root {
            None => builder.with_no_client_auth(),
            Some(cert) => {
                let mut roots = RootCertStore::empty();
                add_certs_from_pem(&mut Cursor::new(cert), &mut roots)?;
                let verifier = if client_auth_optional {
                    WebPkiClientVerifier::builder(roots.into()).allow_unauthenticated()
                } else {
                    WebPkiClientVerifier::builder(roots.into())
                }
                .build()?;
                builder.with_client_cert_verifier(verifier)
            }
        };

        let (cert, key) = load_identity(identity)?;
        let mut config = builder.with_single_cert(cert, key)?;

        config.alpn_protocols.push(ALPN_H2.into());
        Ok(Self {
            inner: Arc::new(config),
        })
    }

    pub(crate) async fn accept<IO>(&self, io: IO) -> Result<TlsStream<IO>, crate::Error>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let acceptor = RustlsAcceptor::from(self.inner.clone());
        acceptor.accept(io).await.map_err(Into::into)
    }
}

impl fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsAcceptor").finish()
    }
}
