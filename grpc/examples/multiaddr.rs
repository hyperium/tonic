use std::any::Any;

use grpc::credentials::InsecureChannelCredentials;
use grpc::service::{Message, Request, Response, Service};
use grpc::{client::ChannelOptions, inmemory};
use tokio_stream::StreamExt;
use tonic::async_trait;

struct Handler {
    id: String,
}

#[derive(Debug)]
struct MyReqMessage(String);

#[derive(Debug)]
struct MyResMessage(String);

#[async_trait]
impl Service for Handler {
    async fn call(&self, method: String, request: Request) -> Response {
        let id = self.id.clone();
        let mut stream = request.into_inner();
        let output = async_stream::try_stream! {
            while let Some(req) = stream.next().await {
                yield Box::new(MyResMessage(format!(
                    "Server {}: responding to: {}; msg: {}",
                    id, method, (req as Box<dyn Any>).downcast_ref::<MyReqMessage>().unwrap().0,
                ))) as Box<dyn Message>;
            }
        };

        Response::new(Box::pin(output))
    }
}

#[tokio::main]
async fn main() {
    inmemory::reg();

    // Spawn the first server.
    let lis1 = inmemory::Listener::new();
    let mut srv = grpc::server::Server::new();
    srv.set_handler(Handler { id: lis1.id() });
    let lis1_clone = lis1.clone();
    tokio::task::spawn(async move {
        srv.serve(&lis1_clone).await;
        println!("serve returned for listener 1!");
    });

    // Spawn the second server.
    let lis2 = inmemory::Listener::new();
    let mut srv = grpc::server::Server::new();
    srv.set_handler(Handler { id: lis2.id() });
    let lis2_clone = lis2.clone();
    tokio::task::spawn(async move {
        srv.serve(&lis2_clone).await;
        println!("serve returned for listener 2!");
    });

    // Spawn the third server.
    let lis3 = inmemory::Listener::new();
    let mut srv = grpc::server::Server::new();
    srv.set_handler(Handler { id: lis3.id() });
    let lis3_clone = lis3.clone();
    tokio::task::spawn(async move {
        srv.serve(&lis3_clone).await;
        println!("serve returned for listener 3!");
    });

    let target = String::from("inmemory:///dummy");
    println!("Creating channel for {target}");
    let chan_opts = ChannelOptions::default();
    let chan = grpc::client::Channel::new(
        target.as_str(),
        InsecureChannelCredentials::new(),
        chan_opts,
    );

    let outbound = async_stream::stream! {
        yield Box::new(MyReqMessage("My Request 1".to_string())) as Box<dyn Message>;
        yield Box::new(MyReqMessage("My Request 2".to_string()));
        yield Box::new(MyReqMessage("My Request 3".to_string()));
    };

    let req = Request::new(Box::pin(outbound));
    let res = chan.call("/some/method".to_string(), req).await;
    let mut res = res.into_inner();

    while let Some(resp) = res.next().await {
        println!(
            "CALL RESPONSE: {}",
            (resp.unwrap() as Box<dyn Any>)
                .downcast_ref::<MyResMessage>()
                .unwrap()
                .0,
        );
    }

    lis1.close().await;
    lis2.close().await;
    lis3.close().await;
}
