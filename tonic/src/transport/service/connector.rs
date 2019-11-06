use super::io::BoxedIo;
#[cfg(feature = "tls")]
use super::tls::TlsConnector;
use http::Uri;
use hyper::client::connect::HttpConnector;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower_make::MakeConnection;
use tower_service::Service;

#[cfg(not(feature = "tls"))]
pub(crate) fn connector() -> HttpConnector {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_nodelay(true);
    http
}

#[cfg(feature = "tls")]
pub(crate) fn connector(tls: Option<TlsConnector>) -> Connector {
    Connector::new(tls)
}

pub(crate) struct Connector {
    http: HttpConnector,
    #[cfg(feature = "tls")]
    tls: Option<TlsConnector>,
}

impl Connector {
    #[cfg(feature = "tls")]
    pub(crate) fn new(tls: Option<TlsConnector>) -> Self {
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_nodelay(true);

        Self { http, tls }
    }
}

impl Service<Uri> for Connector {
    type Response = BoxedIo;
    type Error = crate::Error;

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeConnection::poll_ready(&mut self.http, cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connect = MakeConnection::make_connection(&mut self.http, uri);

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
