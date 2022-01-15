pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use futures::Stream;
use std::net::ToSocketAddrs;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tonic::{transport::Server, Request, Response, Status, Streaming};

use pb::{EchoRequest, EchoResponse};

type EchoResult<T> = Result<Response<T>, Status>;
type ResponseStream = Pin<Box<dyn Stream<Item = Result<EchoResponse, Status>> + Send>>;

#[derive(Debug)]
pub struct EchoServer {}

#[tonic::async_trait]
impl pb::echo_server::Echo for EchoServer {
    async fn unary_echo(&self, _: Request<EchoRequest>) -> EchoResult<EchoResponse> {
        Err(Status::unimplemented("not implemented"))
    }

    type ServerStreamingEchoStream = ResponseStream;

    async fn server_streaming_echo(
        &self,
        req: Request<EchoRequest>,
    ) -> EchoResult<Self::ServerStreamingEchoStream> {
        println!("Client connected from: {:?}", req.remote_addr());

        let (tx, rx) = mpsc::channel(100);

        let echo_request = req.into_inner();
        let echo_response = EchoResponse {
            message: echo_request.message,
        };

        tx.send(echo_response).await.unwrap();

        Ok(Response::new(
            Box::pin(ClientResponder(rx)) as Self::ServerStreamingEchoStream
        ))
    }

    async fn client_streaming_echo(
        &self,
        req: Request<Streaming<EchoRequest>>,
    ) -> EchoResult<EchoResponse> {
        println!("Client connected from: {:?}", req.remote_addr());

        let mut receiving_stream = req.into_inner();

        let incoming_message = receiving_stream.message().await;

        // only echo first request in stream
        return match incoming_message? {
            Some(echo_request) => {
                let echo_response = EchoResponse {
                    message: echo_request.message,
                };

                println!("Server will echo: {}", echo_response.message);
                Ok(Response::new(echo_response))
            }
            None => Err(Status::unavailable("No message received")),
        };
    }

    type BidirectionalStreamingEchoStream = ResponseStream;

    async fn bidirectional_streaming_echo(
        &self,
        req: Request<Streaming<EchoRequest>>,
    ) -> EchoResult<Self::BidirectionalStreamingEchoStream> {
        println!("Client connected from: {:?}", req.remote_addr());

        let (tx, rx) = mpsc::channel(100);

        tokio::spawn(async move {
            let mut receiving_stream = req.into_inner();

            // echo all requests in incoming stream
            loop {
                let incoming_message = receiving_stream.message().await;

                match incoming_message {
                    Ok(Some(echo_request)) => {
                        let echo_response = EchoResponse {
                            message: echo_request.message,
                        };

                        tx.send(echo_response).await.unwrap();
                    }
                    Ok(None) => {
                        println!("No message passed");
                        break;
                    }
                    Err(status) => {
                        println!("Received status {}", status);
                        break;
                    }
                }
            }

            println!("Stream finished");
        });

        Ok(Response::new(
            Box::pin(ClientResponder(rx)) as Self::BidirectionalStreamingEchoStream
        ))
    }
}

struct ClientResponder(mpsc::Receiver<EchoResponse>);

impl Stream for ClientResponder {
    type Item = Result<EchoResponse, Status>;

    fn poll_next(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let outgoing_poll = Pin::new(&mut self.get_mut().0).poll_recv(ctx);

        return match outgoing_poll {
            Poll::Ready(Some(echo)) => {
                println!("Server will echo: {}", echo.message);

                Poll::Ready(Some(Ok(echo)))
            }
            Poll::Ready(None) => Poll::Ready(Some(Err(Status::unavailable("empty stream")))),
            Poll::Pending => Poll::Pending,
        };
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = EchoServer {};
    Server::builder()
        .add_service(pb::echo_server::EchoServer::new(server))
        .serve("[::1]:50051".to_socket_addrs().unwrap().next().unwrap())
        .await
        .unwrap();

    Ok(())
}
