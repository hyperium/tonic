use structopt::StructOpt;
use tonic::{Code, Request, Response, Status, Server};
use std::pin::Pin;
use tonic_interop::pb::*;

#[derive(Default, Clone)]
pub struct TestService {
    data: String,
}

type Result<T> = std::result::Result<Response<T>, Status>;
type Streaming<T> = Request<tonic::Streaming<T>>;
type Stream<T> = Pin<Box<dyn futures_core::Stream<Item = std::result::Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl tonic_interop::pb::TestService for TestService {
    async fn empty_call(&self, _request: Request<Empty>) -> Result<Empty> {
        println!("empty_call");
        Ok(Response::new(Empty {}))
    }

    async fn unary_call(
        &self,
        request: Request<SimpleRequest>,
    ) -> Result<SimpleResponse> {
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

        let res = SimpleResponse {
            payload: Some(Payload {
                body: vec![0; res_size],
                ..Default::default()
            }),
            ..Default::default()
        };

        Ok(Response::new(res))
    }

    async fn cacheable_unary_call(&self, _: Request<SimpleRequest>) -> Result<SimpleResponse> {
        unimplemented!()
    }

    type StreamingOutputCallStream = Stream<StreamingOutputCallResponse>;

    async fn streaming_output_call(&self, _: Request<StreamingOutputCallRequest>) -> Result<Self::StreamingOutputCallStream> {
        unimplemented!()
    }

    async fn streaming_input_call(&self, _: Streaming<StreamingInputCallRequest>) -> Result<StreamingInputCallResponse> {
        unimplemented!()
    }

    type FullDuplexCallStream = Stream<StreamingOutputCallResponse>;

    async fn full_duplex_call(&self, _: Streaming<StreamingOutputCallRequest>) -> Result<Self::FullDuplexCallStream> {
        unimplemented!()
    }

    type HalfDuplexCallStream = Stream<StreamingOutputCallResponse>;

    async fn half_duplex_call(&self, _: Streaming<StreamingOutputCallRequest>) -> Result<Self::HalfDuplexCallStream> {
        unimplemented!()
    }

    async fn unimplemented_call(&self, _: Request<Empty>) -> Result<Empty> {
        unimplemented!()
    }
}

#[derive(StructOpt)]
struct Opts {
    #[structopt(long)]
    use_tls: bool,
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
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
