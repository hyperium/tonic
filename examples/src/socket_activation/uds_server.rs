//! Greeter server demonstrating systemd socket activation over a Unix domain
#![cfg_attr(not(unix), allow(unused_imports))]

use tonic::{Request, Response, Status, transport::Server};

#[cfg(unix)]
use tonic::transport::server::UnixIncoming;

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

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
        println!("Got a request: {request:?}");

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("TONIC_UDS_PATH")
        .unwrap_or_else(|_| "/tmp/tonic/socket-activation.sock".to_string());
    let greeter = MyGreeter::default();

    if std::env::var_os("LISTEN_FDS").is_some() {
        println!(
            "socket activation detected: adopting socket-activated descriptor for {path} if available"
        );
    } else {
        println!("no socket activation: binding {path} directly");
        if let Some(parent) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Remove a stale socket file from a previous direct run.
        let _ = std::fs::remove_file(&path);
    }

    let incoming = UnixIncoming::bind(&path)?;

    Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve_with_incoming(incoming)
        .await?;

    Ok(())
}

#[cfg(not(unix))]
fn main() {
    panic!("The socket-activation UDS example only works on unix systems!");
}
