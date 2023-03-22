pub mod pb {
    tonic::include_proto!("/grpc.examples.unaryecho");
}

use hyper::server::conn::Http;
use pb::{EchoRequest, EchoResponse};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::{
    rustls::{Certificate, PrivateKey, ServerConfig},
    TlsAcceptor,
};
use tonic::{transport::Server, Request, Response, Status};
use tower_http::ServiceBuilderExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let certs = {
        let fd = std::fs::File::open(data_dir.join("tls/server.pem"))?;
        let mut buf = std::io::BufReader::new(&fd);
        rustls_pemfile::certs(&mut buf)?
            .into_iter()
            .map(Certificate)
            .collect()
    };
    let key = {
        let fd = std::fs::File::open(data_dir.join("tls/server.key"))?;
        let mut buf = std::io::BufReader::new(&fd);
        rustls_pemfile::pkcs8_private_keys(&mut buf)?
            .into_iter()
            .map(PrivateKey)
            .next()
            .unwrap()

        // let key = std::fs::read(data_dir.join("tls/server.key"))?;
        // PrivateKey(key)
    };

    let mut tls = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    tls.alpn_protocols = vec![b"h2".to_vec()];

    let server = EchoServer::default();

    let svc = Server::builder()
        .add_service(pb::echo_server::EchoServer::new(server))
        .into_service();

    let mut http = Http::new();
    http.http2_only(true);

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

            http.serve_connection(conn, svc).await.unwrap();
        });
    }
}

#[derive(Debug)]
struct ConnInfo {
    addr: std::net::SocketAddr,
    certificates: Vec<Certificate>,
}

type EchoResult<T> = Result<Response<T>, Status>;

#[derive(Default)]
pub struct EchoServer;

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
