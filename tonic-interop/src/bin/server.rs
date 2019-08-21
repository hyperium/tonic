use tokio::net::TcpListener;
use tonic::{Code, Request, Response, Status};
use tower_h2::Server;

pub mod pb {
    #![allow(dead_code)]
    #![allow(unused_imports)]
    include!(concat!(env!("OUT_DIR"), "/grpc.testing.rs"));
    tonic::client!(service = "grpc.testing.TestService", proto = "self");
}

use pb::*;

#[derive(Default, Clone)]
pub struct TestService {
    data: String,
}

#[tonic::server(service = "grpc.testing.TestService", proto = "pb")]
impl TestService {
    pub async fn empty_call(&self, request: Request<Empty>) -> Result<Response<Empty>, Status> {
        println!("empty_call; REQUEST={:?}", request);
        Ok(Response::new(Empty {}))
    }

    pub async fn unary_call(
        &self,
        request: Request<SimpleRequest>,
    ) -> Result<Response<SimpleResponse>, Status> {
        println!("unary_call; REQUEST={:?}", request);

        let req = request.into_inner();

        if let Some(echo_status) = req.response_status {
            let status = Status::new(Code::from_i32(echo_status.code), echo_status.message);
            return Err(status);
        }

        let res_size = if req.response_size >= 0 {
            req.response_size as usize
        } else {
            let status = Status::new(Code::InvalidArgument, "response_size cannot be negative");
            return Err(status);
        };

        let res = pb::SimpleResponse {
            payload: Some(pb::Payload {
                body: vec![0; res_size],
                ..Default::default()
            }),
            ..Default::default()
        };

        Ok(Response::new(res))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();
    let addr = "127.0.0.1:10000".parse().unwrap();
    let mut bind = TcpListener::bind(&addr)?;

    let greeter = TestService::default();
    let mut server = Server::new(TestServiceServer::new(greeter), Default::default());

    while let Ok((sock, _addr)) = bind.accept().await {
        println!("new connection");
        if let Err(e) = sock.set_nodelay(true) {
            return Err(e.into());
        }

        if let Err(e) = server.serve(sock).await {
            println!("H2 ERROR: {}", e);
        }
    }

    Ok(())
}
