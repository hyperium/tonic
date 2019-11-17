pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use futures::Stream;
use pb::{EchoRequest, EchoResponse};
use std::pin::Pin;
use tonic::{body::BoxBody, transport::Server, Request, Response, Status, Streaming};
use tower::Service;

type EchoResult<T> = Result<Response<T>, Status>;
type ResponseStream = Pin<Box<dyn Stream<Item = Result<EchoResponse, Status>> + Send + Sync>>;

#[derive(Default)]
pub struct EchoServer;

#[tonic::async_trait]
impl pb::server::Echo for EchoServer {
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

    async fn client_streaming_echo(
        &self,
        _: Request<Streaming<EchoRequest>>,
    ) -> EchoResult<EchoResponse> {
        Err(Status::unimplemented("not implemented"))
    }

    type BidirectionalStreamingEchoStream = ResponseStream;

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

    Server::builder()
        .interceptor_fn(move |svc, req| {
            let auth_header = req.headers().get("authorization").clone();

            let authed = if let Some(auth_header) = auth_header {
                auth_header == "Bearer some-secret-token"
            } else {
                false
            };

            let fut = svc.call(req);

            async move {
                if authed {
                    fut.await
                } else {
                    // Cancel the inner future since we never await it
                    // the IO never gets registered.
                    drop(fut);
                    let res = http::Response::builder()
                        .header("grpc-status", "16")
                        .body(BoxBody::empty())
                        .unwrap();
                    Ok(res)
                }
            }
        })
        .add_service(pb::server::EchoServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}
