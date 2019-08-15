#![feature(async_await)]

use std::time::Duration;
use tokio::{timer::Delay, net::TcpListener};
use tonic::{Request, Response, Status};
use tower_h2::Server;

mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HelloRequest {
        #[prost(string, tag = "1")]
        pub name: std::string::String,
    }
    /// The response message containing the greetings
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HelloReply {
        #[prost(string, tag = "1")]
        pub message: std::string::String,
    }
}

#[derive(Default, Clone)]
pub struct MyGreeter {
    data: String,
}

#[tonic::server(service = "helloworld.Greeter", proto = "proto")]
impl MyGreeter {
    pub async fn say_hello(&self, request: Request<proto::HelloRequest>) -> Result<Response<proto::HelloReply>, Status> {
        println!("Got a request: {:?}", request);

        let string = &self.data;

        let when = tokio::clock::now() + Duration::from_millis(100);
        Delay::new(when).await;

        println!("My data: {:?}", string);

        Delay::new(when).await;
        
        let reply = HelloReply {
            message: "Zomg, it works!".into(),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let mut bind = TcpListener::bind(&addr)?;

    let greeter = MyGreeter::default();
    let mut server = Server::new(GrpcServer::new(greeter), Default::default());

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
