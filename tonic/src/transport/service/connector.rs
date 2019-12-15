use super::io::BoxedIo;
#[cfg(feature = "tls")]
use super::tls::TlsConnector;
use http::Uri;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower_make::MakeConnection;
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

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeConnection::poll_ready(&mut self.inner, cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connect = self.inner.make_connection(uri);

        #[cfg(feature = "tls")]
        let tls = self.tls.clone();

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
