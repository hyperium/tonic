#![feature(async_await)]

use std::time::Duration;
use tokio::{net::TcpListener, timer::Delay};
use tonic::{Request, Response, Status};
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
        println!("REQUEST={:?}", request);
        Ok(Response::new(Empty {}))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:10000".parse().unwrap();
    let mut bind = TcpListener::bind(&addr)?;

    let greeter = TestService::default();
    let mut server = Server::new(TestServiceServer::new(greeter), Default::default());

    while let Ok((sock, _addr)) = bind.accept().await {
        if let Err(e) = sock.set_nodelay(true) {
            return Err(e.into());
        }

        if let Err(e) = server.serve(sock).await {
            println!("H2 ERROR: {}", e);
        }
    }

    Ok(())
}
