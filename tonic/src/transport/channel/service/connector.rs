use super::BoxedIo;
#[cfg(feature = "tls")]
use super::TlsConnector;
use crate::transport::channel::BoxFuture;
use crate::ConnectError;
use http::Uri;
#[cfg(feature = "tls")]
use std::fmt;
use std::task::{Context, Poll};

use hyper::rt;

#[cfg(feature = "tls")]
use hyper_util::rt::TokioIo;
use tower_service::Service;

pub(crate) struct Connector<C> {
    inner: C,
    #[cfg(feature = "tls")]
    tls: Option<TlsConnector>,
}

impl<C> Connector<C> {
    pub(crate) fn new(inner: C, #[cfg(feature = "tls")] tls: Option<TlsConnector>) -> Self {
        Self {
            inner,
            #[cfg(feature = "tls")]
            tls,
        }
    }
}

impl<C> Service<Uri> for Connector<C>
where
    C: Service<Uri>,
    C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
    C::Future: Send + 'static,
    crate::Error: From<C::Error> + Send + 'static,
{
    type Response = BoxedIo;
    type Error = ConnectError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready(cx)
            .map_err(|err| ConnectError(From::from(err)))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        #[cfg(feature = "tls")]
        let tls = self.tls.clone();

        #[cfg(feature = "tls")]
        let is_https = uri.scheme_str() == Some("https");
        let connect = self.inner.call(uri);

        Box::pin(async move {
            async {
                let io = connect.await?;

                #[cfg(feature = "tls")]
                if is_https {
                    return if let Some(tls) = tls {
                        let io = tls.connect(TokioIo::new(io)).await?;
                        Ok(io)
                    } else {
                        Err(HttpsUriWithoutTlsSupport(()).into())
                    };
                }

                Ok::<_, crate::Error>(BoxedIo::new(io))
            }
            .await
            .map_err(ConnectError)
        })
    }
}

/// Error returned when trying to connect to an HTTPS endpoint without TLS enabled.
#[cfg(feature = "tls")]
#[derive(Debug)]
pub(crate) struct HttpsUriWithoutTlsSupport(());

#[cfg(feature = "tls")]
impl fmt::Display for HttpsUriWithoutTlsSupport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Connecting to HTTPS without TLS enabled")
    }
}

// std::error::Error only requires a type to impl Debug and Display
#[cfg(feature = "tls")]
impl std::error::Error for HttpsUriWithoutTlsSupport {}
