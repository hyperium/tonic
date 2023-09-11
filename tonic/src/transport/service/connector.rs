use super::super::BoxFuture;
use super::io::BoxedIo;
#[cfg(feature = "tls")]
use super::tls::TlsConnector;
use http::Uri;
use std::fmt;
use std::task::{Context, Poll};
use tower::make::MakeConnection;
use tower_service::Service;

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
    pub(crate) fn new(inner: C, tls: Option<TlsConnector>) -> Self {
        Self { inner, tls }
    }

    #[cfg(feature = "tls-roots-common")]
    fn tls_or_default(&self, scheme: Option<&str>, host: Option<&str>) -> Option<TlsConnector> {
        if self.tls.is_some() {
            return self.tls.clone();
        }

        let host = match (scheme, host) {
            (Some("https"), Some(host)) => host,
            _ => return None,
        };

        TlsConnector::new(None, None, host).ok()
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
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeConnection::poll_ready(&mut self.inner, cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        #[cfg(all(feature = "tls", not(feature = "tls-roots-common")))]
        let tls = self.tls.clone();

        #[cfg(feature = "tls-roots-common")]
        let tls = self.tls_or_default(uri.scheme_str(), uri.host());

        #[cfg(feature = "tls")]
        let is_https = uri.scheme_str() == Some("https");
        let connect = self.inner.make_connection(uri);

        Box::pin(async move {
            let io = connect.await?;

            #[cfg(feature = "tls")]
            {
                if let Some(tls) = tls {
                    if is_https {
                        let conn = tls.connect(io).await?;
                        return Ok(BoxedIo::new(conn));
                    } else {
                        return Ok(BoxedIo::new(io));
                    }
                } else if is_https {
                    return Err(HttpsUriWithoutTlsSupport(()).into());
                }
            }

            Ok(BoxedIo::new(io))
        })
    }
}

/// Error returned when trying to connect to an HTTPS endpoint without TLS enabled.
#[derive(Debug)]
pub(crate) struct HttpsUriWithoutTlsSupport(());

impl fmt::Display for HttpsUriWithoutTlsSupport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Connecting to HTTPS without TLS enabled")
    }
}

// std::error::Error only requires a type to impl Debug and Display
impl std::error::Error for HttpsUriWithoutTlsSupport {}
