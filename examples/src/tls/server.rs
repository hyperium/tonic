pub mod pb {
    tonic::include_proto!("/grpc.examples.echo");
}

use futures::Stream;
use pb::{EchoRequest, EchoResponse};
use std::pin::Pin;
use tonic::{
    transport::{Identity, Server, ServerTlsConfig},
    MessageStream, Request, Response, Status,
};

type EchoResult<T> = Result<Response<T>, Status>;
type ResponseStream = Pin<Box<dyn Stream<Item = Result<EchoResponse, Status>> + Send + Sync>>;

#[derive(Default)]
pub struct EchoServer;

#[tonic::async_trait]
impl pb::echo_server::Echo for EchoServer {
    async fn unary_echo(&self, request: Request<EchoRequest>) -> EchoResult<EchoResponse> {
        let message = request.into_inner().message;
        Ok(Response::new(EchoResponse { message }))
    }

    type ServerStreamingEchoStream = ResponseStream;

    async fn server_streaming_echo(
        &self,
        _: Request<EchoRequest>,
    ) -> EchoResult<Self::ServerStreamingEchoStream> {
        Err(Status::unimplemented("not implemented"))
    }

    async fn client_streaming_echo<S: MessageStream<Message = EchoRequest>>(
        &self,
        _: Request<S>,
    ) -> EchoResult<EchoResponse> {
        Err(Status::unimplemented("not implemented"))
    }

    type BidirectionalStreamingEchoStream = ResponseStream;

    async fn bidirectional_streaming_echo<S: MessageStream<Message = EchoRequest>>(
        &self,
        _: Request<S>,
    ) -> EchoResult<Self::BidirectionalStreamingEchoStream> {
        Err(Status::unimplemented("not implemented"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cert = tokio::fs::read("examples/data/tls/server.pem").await?;
    let key = tokio::fs::read("examples/data/tls/server.key").await?;

    let identity = Identity::from_pem(cert, key);

    let addr = "[::1]:50051".parse().unwrap();
    let server = EchoServer::default();

    Server::builder()
        .tls_config(ServerTlsConfig::new().identity(identity))?
        .add_service(pb::echo_server::EchoServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}
