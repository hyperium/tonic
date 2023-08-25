use std::future::Future;

use tokio_util::sync::CancellationToken;
use tonic::{transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

use tokio::select;
use tokio::time::sleep;
use tokio::time::Duration;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        let remote_addr = request.remote_addr();
        let request_future = async move {
            println!("Got a request from {:?}", request.remote_addr());

            // Take a long time to complete request for the client to cancel early
            sleep(Duration::from_secs(10)).await;

            let reply = hello_world::HelloReply {
                message: format!("Hello {}!", request.into_inner().name),
            };

            Ok(Response::new(reply))
        };
        let cancellation_future = async move {
            println!("Request from {:?} cancelled by client", remote_addr);
            // If this future is executed it means the request future was dropped,
            // so it doesn't actually matter what is returned here
            Err(Status::cancelled("Request cancelled by client"))
        };
        with_cancellation_handler(request_future, cancellation_future).await
    }
}

async fn with_cancellation_handler<FRequest, FCancellation>(
    request_future: FRequest,
    cancellation_future: FCancellation,
) -> Result<Response<HelloReply>, Status>
where
    FRequest: Future<Output = Result<Response<HelloReply>, Status>> + Send + 'static,
    FCancellation: Future<Output = Result<Response<HelloReply>, Status>> + Send + 'static,
{
    let token = CancellationToken::new();
    // Will call token.cancel() when the future is dropped, such as when the client cancels the request
    let _drop_guard = token.clone().drop_guard();
    let select_task = tokio::spawn(async move {
        // Can select on token cancellation on any cancellable future while handling the request,
        // allowing for custom cleanup code or monitoring
        select! {
            res = request_future => res,
            _ = token.cancelled() => cancellation_future.await,
        }
    });

    select_task.await.unwrap()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    println!("GreeterServer listening on {}", addr);

    Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
