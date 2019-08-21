use std::time::Duration;
use tokio::{net::TcpListener, timer::Delay};
use tonic::{Request, Response, Status};
use tower_h2::Server;

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
}

#[derive(Default, Clone)]
pub struct MyGreeter {
    data: String,
}

#[tonic::server(service = "helloworld.Greeter", proto = "hello_world")]
impl MyGreeter {
    pub async fn say_hello(
        &self,
        request: Request<hello_world::HelloRequest>,
    ) -> Result<Response<hello_world::HelloReply>, Status> {
        println!("Got a request: {:?}", request);

        let string = &self.data;

        let when = tokio::clock::now() + Duration::from_millis(100);
        Delay::new(when).await;

        println!("My data: {:?}", string);

        Delay::new(when).await;

        let reply = hello_world::HelloReply {
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
    let mut server = Server::new(GreeterServer::new(greeter), Default::default());

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
