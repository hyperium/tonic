#![cfg_attr(not(windows), allow(unused_imports))]

#[cfg(windows)]
use std::io;
#[cfg(windows)]
use std::pin::Pin;
#[cfg(windows)]
use std::task::{Context, Poll};
#[cfg(windows)]
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
use tokio_stream::StreamExt;
#[cfg(windows)]
use tonic::transport::server::NamedPipeIncoming;
use tonic::{transport::Server, Request, Response, Status};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{
    greeter_server::{Greeter, GreeterServer},
    HelloReply, HelloRequest,
};

#[derive(Default)]
pub struct MyGreeter;

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        #[cfg(windows)]
        {
            let conn_info = request
                .extensions()
                .get::<PipeConnectInfo>()
                .expect("connect info missing");
            println!(
                "Got a request {request:?} on pipe {} (connection {})",
                conn_info.pipe_name, conn_info.connection_id
            );
        }
        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pipe_name = r"\\.\pipe\tonic\helloworld".to_string();
    let greeter = MyGreeter::default();

    println!("gRPC server listening on {}", pipe_name);
    let mut next_id: u64 = 1;
    let incoming = NamedPipeIncoming::new(&pipe_name).map(move |item| {
        item.map(|server| {
            let id = next_id;
            next_id += 1;
            PipeConn::new(server, PipeConnectInfo::new(&pipe_name, id))
        })
    });
    Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve_with_incoming(incoming)
        .await?;
    Ok(())
}

#[cfg(not(windows))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    unimplemented!("Named pipes are only supported on Windows");
}

#[cfg(windows)]
#[derive(Debug, Clone)]
struct PipeConnectInfo {
    pipe_name: String,
    connection_id: u64,
}

#[cfg(windows)]
impl PipeConnectInfo {
    fn new(pipe_name: &str, connection_id: u64) -> Self {
        Self {
            pipe_name: pipe_name.to_string(),
            connection_id,
        }
    }
}

#[cfg(windows)]
struct PipeConn {
    inner: NamedPipeServer,
    info: PipeConnectInfo,
}

#[cfg(windows)]
impl PipeConn {
    fn new(inner: NamedPipeServer, info: PipeConnectInfo) -> Self {
        Self { inner, info }
    }
}

#[cfg(windows)]
impl tonic::transport::server::Connected for PipeConn {
    type ConnectInfo = PipeConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.info.clone()
    }
}

#[cfg(windows)]
impl AsyncRead for PipeConn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

#[cfg(windows)]
impl AsyncWrite for PipeConn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
