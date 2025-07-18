use std::any::Any;

use futures_util::stream::StreamExt;
use grpc::service::{Message, Request, Response, Service};
use grpc::{client::ChannelOptions, inmemory};
use tonic::async_trait;

struct Handler {}

#[derive(Debug)]
struct MyReqMessage(String);

impl Message for MyReqMessage {}

#[derive(Debug)]
struct MyResMessage(String);
impl Message for MyResMessage {}

#[async_trait]
impl Service for Handler {
    async fn call(&self, method: String, request: Request) -> Response {
        let mut stream = request.into_inner();
        let output = async_stream::try_stream! {
            while let Some(req) = stream.next().await {
                yield Box::new(MyResMessage(format!(
                    "Server: responding to: {}; msg: {}",
                    method, (req as Box<dyn Any>).downcast_ref::<MyReqMessage>().unwrap().0,
                ))) as Box<dyn Message>;
            }
        };

        Response::new(Box::pin(output))
    }
}

#[tokio::main]
async fn main() {
    inmemory::reg();

    // Spawn the server.
    let lis = inmemory::Listener::new();
    let mut srv = grpc::server::Server::new();
    srv.set_handler(Handler {});
    let lis_clone = lis.clone();
    tokio::task::spawn(async move {
        srv.serve(&lis_clone).await;
        println!("serve returned for listener 1!");
    });

    println!("Creating channel for {}", lis.target());
    let chan_opts = ChannelOptions::default();
    let chan = grpc::client::Channel::new(lis.target().as_str(), None, chan_opts);

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
    lis.close().await;
}
