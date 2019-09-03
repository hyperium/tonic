use structopt::StructOpt;
use tonic::transport::Server;
use tonic::{Code, Request, Response, Status};

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
    pub async fn empty_call(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        println!("empty_call");
        Ok(Response::new(Empty {}))
    }

    pub async fn unary_call(
        &self,
        request: Request<SimpleRequest>,
    ) -> Result<Response<SimpleResponse>, Status> {
        println!("unary_call");

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

#[derive(StructOpt)]
struct Opts {
    #[structopt(long)]
    use_tls: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = Opts::from_args();

    pretty_env_logger::init();
    let addr = "127.0.0.1:10000".parse().unwrap();

    let greeter = TestService::default();

    let mut builder = Server::builder();

    if matches.use_tls {
        let ca = tokio::fs::read("tonic-interop/data/server1.pem").await?;
        let key = tokio::fs::read("tonic-interop/data/server1.key").await?;
        builder.tls(ca, key);
    }

    builder.serve(addr, TestServiceServer::new(greeter)).await?;

    Ok(())
}
