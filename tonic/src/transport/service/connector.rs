use super::super::BoxFuture;
use super::io::BoxedIo;
#[cfg(feature = "tls")]
use super::tls::TlsConnector;
use http::Uri;
use std::task::{Context, Poll};
use tower::make::MakeConnection;
use tower_service::Service;

#[cfg(not(feature = "tls"))]
pub(crate) fn connector<C>(inner: C) -> Connector<C> {
    Connector::new(inner)
}

#[cfg(feature = "tls")]
pub(crate) fn connector<C>(inner: C, tls: Option<TlsConnector>) -> Connector<C> {
    Connector::new(inner, tls)
}

pub(crate) struct Connector<C> {
    inner: C,
    #[cfg(feature = "tls")]
    tls: Option<TlsConnector>,
    #[cfg(not(feature = "tls"))]
    #[allow(dead_code)]
    tls: Option<()>,
}

impl<C> Connector<C> {
    #[cfg(not(feature = "tls"))]
    pub(crate) fn new(inner: C) -> Self {
        Self { inner, tls: None }
    }

    #[cfg(feature = "tls")]
    fn new(inner: C, tls: Option<TlsConnector>) -> Self {
        Self { inner, tls }
    }

    #[cfg(feature = "tls-roots-common")]
    fn tls_or_default(&self, scheme: Option<&str>, host: Option<&str>) -> Option<TlsConnector> {
        use tokio_rustls::webpki::DNSNameRef;

        if self.tls.is_some() {
            return self.tls.clone();
        }

        match (scheme, host) {
            (Some("https"), Some(host)) => {
                if DNSNameRef::try_from_ascii(host.as_bytes()).is_ok() {
                    TlsConnector::new_with_rustls_cert(None, None, host.to_owned()).ok()
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl<C> Service<Uri> for Connector<C>
where
    C: MakeConnection<Uri>,
    C::Connection: Unpin + Send + 'static,
    C::Future: Send + 'static,
    crate::Error: From<C::Error> + Send + 'static,
{
    type Response = BoxedIo;
    type Error = crate::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeConnection::poll_ready(&mut self.inner, cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        #[cfg(all(feature = "tls", not(feature = "tls-roots-common")))]
        let tls = self.tls.clone();

        #[cfg(feature = "tls-roots-common")]
        let tls = self.tls_or_default(uri.scheme_str(), uri.host());

        let connect = self.inner.make_connection(uri);

        Box::pin(async move {
            let io = connect.await?;

            #[cfg(feature = "tls")]
            {
                if let Some(tls) = tls {
                    let conn = tls.connect(io).await?;
                    return Ok(BoxedIo::new(conn));
                }
            }

            Ok(BoxedIo::new(io))
        })
    }
}
