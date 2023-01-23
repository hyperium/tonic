pub mod pb {
    tonic::include_proto!("grpc.examples.unaryecho");
}

use pb::{EchoRequest, EchoResponse};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status};

type EchoResult<T> = Result<Response<T>, Status>;

#[derive(Default)]
pub struct EchoServer;

#[tonic::async_trait]
impl pb::echo_server::Echo for EchoServer {
    async fn unary_echo(&self, request: Request<EchoRequest>) -> EchoResult<EchoResponse> {
        let certs = request
            .peer_certs()
            .expect("Client did not send its certs!");

        println!("Got {} peer certs!", certs.len());

        let message = request.into_inner().message;
        Ok(Response::new(EchoResponse { message }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let cert = std::fs::read_to_string(data_dir.join("tls/server.pem"))?;
    let key = std::fs::read_to_string(data_dir.join("tls/server.key"))?;
    let server_identity = Identity::from_pem(cert, key);

    let client_ca_cert = std::fs::read_to_string(data_dir.join("tls/client_ca.pem"))?;
    let client_ca_cert = Certificate::from_pem(client_ca_cert);

    let addr = "[::1]:50051".parse().unwrap();
    let server = EchoServer::default();

    let tls = ServerTlsConfig::new()
        .identity(server_identity)
        .client_ca_root(client_ca_cert);

    Server::builder()
        .tls_config(tls)?
        .add_service(pb::echo_server::EchoServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}
