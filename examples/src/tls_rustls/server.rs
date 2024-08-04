pub mod pb {
    tonic::include_proto!("/grpc.examples.unaryecho");
}

use hyper::server::conn::http2::Builder;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    service::TowerToHyperService,
};
use pb::{EchoRequest, EchoResponse};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::{
    rustls::{pki_types::CertificateDer, ServerConfig},
    TlsAcceptor,
};
use tonic::{body::boxed, service::Routes, Request, Response, Result};
use tower::ServiceExt;
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

            http.serve_connection(
                TokioIo::new(conn),
                TowerToHyperService::new(svc.map_request(|req: http::Request<_>| req.map(boxed))),
            )
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

#[derive(Default)]
pub struct EchoServer {}

#[tonic::async_trait]
impl pb::echo_server::Echo for EchoServer {
    async fn unary_echo(&self, request: Request<EchoRequest>) -> Result<Response<EchoResponse>> {
        let conn_info = request.extensions().get::<Arc<ConnInfo>>().unwrap();
        println!(
            "Got a request from: {:?} with certs: {:?}",
            conn_info.addr, conn_info.certificates
        );

        let message = request.into_inner().message;
        Ok(Response::new(EchoResponse { message }))
    }
}
