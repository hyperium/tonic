pub mod pb {
    tonic::include_proto!("grpc.examples.unaryecho");
}

use std::net::SocketAddr;
use tokio::sync::mpsc;
use tonic::{transport::Server, Request, Response, Result};

use pb::{EchoRequest, EchoResponse};

#[derive(Debug)]
pub struct EchoServer {
    addr: SocketAddr,
}

#[tonic::async_trait]
impl pb::echo_server::Echo for EchoServer {
    async fn unary_echo(&self, request: Request<EchoRequest>) -> Result<Response<EchoResponse>> {
        let message = format!("{} (from {})", request.into_inner().message, self.addr);

        Ok(Response::new(EchoResponse { message }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addrs = ["[::1]:50051", "[::1]:50052"];

    let (tx, mut rx) = mpsc::unbounded_channel();

    for addr in &addrs {
        let addr = addr.parse()?;
        let tx = tx.clone();

        let server = EchoServer { addr };
        let serve = Server::builder()
            .add_service(pb::echo_server::EchoServer::new(server))
            .serve(addr);

        tokio::spawn(async move {
            if let Err(e) = serve.await {
                eprintln!("Error = {:?}", e);
            }

            tx.send(()).unwrap();
        });
    }

    rx.recv().await;

    Ok(())
}
