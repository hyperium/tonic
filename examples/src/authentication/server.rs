pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use futures::Stream;
use pb::{EchoRequest, EchoResponse};
use std::pin::Pin;
use tonic::{metadata::MetadataValue, transport::Server, Request, Response, Status, Streaming};
use tokio_stream::wrappers::ReceiverStream;

type EchoResult<T> = Result<Response<T>, Status>;

#[derive(Default)]
pub struct EchoServer;

#[tonic::async_trait]
impl pb::echo_server::Echo for EchoServer {
    async fn unary_echo(&self, request: Request<EchoRequest>) -> EchoResult<EchoResponse> {
        let message = request.into_inner().message;
        Ok(Response::new(EchoResponse { message }))
    }

    type ServerStreamingEchoStream = ReceiverStream<Result<EchoResponse, Status>>;

    async fn server_streaming_echo(
        &self,
        request: Request<EchoRequest>,
    ) -> EchoResult<Self::ServerStreamingEchoStream> {

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let message = request.into_inner().message;
        let data: Vec<_> = (0..100).map(|x| EchoResponse{message: format!("{}: {}",message,x.to_string())}).collect();

        tokio::spawn(async move {
            for echos in data.into_iter() {
                tx.send(Ok(echos.clone())).await.unwrap();
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn client_streaming_echo(
        &self,
        _: Request<Streaming<EchoRequest>>,
    ) -> EchoResult<EchoResponse> {
        Err(Status::unimplemented("not implemented"))
    }

    type BidirectionalStreamingEchoStream = ReceiverStream<Result<EchoResponse, Status>>;

    async fn bidirectional_streaming_echo(
        &self,
        _: Request<Streaming<EchoRequest>>,
    ) -> EchoResult<Self::BidirectionalStreamingEchoStream> {
        Err(Status::unimplemented("not implemented"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let server = EchoServer::default();

    let svc = pb::echo_server::EchoServer::with_interceptor(server, check_auth);

    Server::builder().add_service(svc).serve(addr).await?;

    Ok(())
}

fn check_auth(req: Request<()>) -> Result<Request<()>, Status> {
    let token = MetadataValue::from_str("Bearer some-secret-token").unwrap();

    match req.metadata().get("authorization") {
        Some(t) if token == t => Ok(req),
        _ => Err(Status::unauthenticated("No valid auth token")),
    }
}
