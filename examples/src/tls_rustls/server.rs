pub mod pb {
    tonic::include_proto!("/grpc.examples.unaryecho");
}

use http_body_util::BodyExt;
use hyper::server::conn::http2::Builder;
use hyper_util::rt::{TokioExecutor, TokioIo};
use pb::{EchoRequest, EchoResponse};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::{
    rustls::{pki_types::CertificateDer, ServerConfig},
    TlsAcceptor,
};
use tonic::{body::BoxBody, service::Routes, Request, Response, Status};
use tower::{BoxError, ServiceExt};
use tower_http::ServiceBuilderExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let certs = {
        let fd = std::fs::File::open(data_dir.join("tls/server.pem"))?;
        let mut buf = std::io::BufReader::new(&fd);
        rustls_pemfile::certs(&mut buf).collect::<Result<Vec<_>, _>>()?
    };
    let key = {
        let fd = std::fs::File::open(data_dir.join("tls/server.key"))?;
        let mut buf = std::io::BufReader::new(&fd);
        rustls_pemfile::private_key(&mut buf)?.unwrap()
    };

    let mut tls = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    tls.alpn_protocols = vec![b"h2".to_vec()];

    let server = EchoServer::default();

    let svc = Routes::new(pb::echo_server::EchoServer::new(server));

    let http = Builder::new(TokioExecutor::new());

    let listener = TcpListener::bind("[::1]:50051").await?;
    let tls_acceptor = TlsAcceptor::from(Arc::new(tls));

    loop {
        let (conn, addr) = match listener.accept().await {
            Ok(incoming) => incoming,
            Err(e) => {
                eprintln!("Error accepting connection: {}", e);
                continue;
            }
        };

        let http = http.clone();
        let tls_acceptor = tls_acceptor.clone();
        let svc = svc.clone();

        tokio::spawn(async move {
            let mut certificates = Vec::new();

            let conn = tls_acceptor
                .accept_with(conn, |info| {
                    if let Some(certs) = info.peer_certificates() {
                        for cert in certs {
                            certificates.push(cert.clone());
                        }
                    }
                })
                .await
                .unwrap();

            let svc = tower::ServiceBuilder::new()
                .add_extension(Arc::new(ConnInfo { addr, certificates }))
                .service(svc);

            http.serve_connection(TokioIo::new(conn), TowerToHyperService::new(svc))
                .await
                .unwrap();
        });
    }
}

#[derive(Debug)]
struct ConnInfo {
    addr: std::net::SocketAddr,
    certificates: Vec<CertificateDer<'static>>,
}

type EchoResult<T> = Result<Response<T>, Status>;

#[derive(Default)]
pub struct EchoServer {}

#[tonic::async_trait]
impl pb::echo_server::Echo for EchoServer {
    async fn unary_echo(&self, request: Request<EchoRequest>) -> EchoResult<EchoResponse> {
        let conn_info = request.extensions().get::<Arc<ConnInfo>>().unwrap();
        println!(
            "Got a request from: {:?} with certs: {:?}",
            conn_info.addr, conn_info.certificates
        );

        let message = request.into_inner().message;
        Ok(Response::new(EchoResponse { message }))
    }
}

/// An adaptor which converts a [`tower::Service`] to a [`hyper::service::Service`].
///
/// The [`hyper::service::Service`] trait is used by hyper to handle incoming requests,
/// and does not support the `poll_ready` method that is used by tower services.
///
/// This is provided here because the equivalent adaptor in hyper-util does not support
/// tonic::body::BoxBody bodies.
#[derive(Debug, Clone)]
struct TowerToHyperService<S> {
    service: S,
}

impl<S> TowerToHyperService<S> {
    /// Create a new `TowerToHyperService` from a tower service.
    fn new(service: S) -> Self {
        Self { service }
    }
}

impl<S> hyper::service::Service<hyper::Request<hyper::body::Incoming>> for TowerToHyperService<S>
where
    S: tower::Service<hyper::Request<BoxBody>> + Clone,
    S::Error: Into<BoxError> + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = TowerToHyperServiceFuture<S, hyper::Request<BoxBody>>;

    fn call(&self, req: hyper::Request<hyper::body::Incoming>) -> Self::Future {
        let req = req.map(|incoming| {
            incoming
                .map_err(|err| Status::from_error(err.into()))
                .boxed_unsync()
        });
        TowerToHyperServiceFuture {
            future: self.service.clone().oneshot(req),
        }
    }
}

/// Future returned by [`TowerToHyperService`].
#[derive(Debug)]
#[pin_project::pin_project]
struct TowerToHyperServiceFuture<S, R>
where
    S: tower::Service<R>,
{
    #[pin]
    future: tower::util::Oneshot<S, R>,
}

impl<S, R> std::future::Future for TowerToHyperServiceFuture<S, R>
where
    S: tower::Service<R>,
    S::Error: Into<BoxError> + 'static,
{
    type Output = Result<S::Response, BoxError>;

    #[inline]
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.project().future.poll(cx).map_err(Into::into)
    }
}
