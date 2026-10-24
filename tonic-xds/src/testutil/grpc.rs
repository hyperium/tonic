/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! Test utilities for gRPC servers and clients.
use std::error::Error;
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::oneshot};
use tonic::server::NamedService;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint, Server, ServerTlsConfig};
use tonic::{Request, Response, Status};

pub(crate) use crate::testutil::proto::helloworld::{
    HelloReply, HelloRequest,
    greeter_client::GreeterClient,
    greeter_server::{Greeter, GreeterServer},
};

#[derive(Default)]
struct MyGreeter {
    msg: String,
}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {
        Ok(Response::new(HelloReply {
            message: format!("{}: {}", self.msg, req.into_inner().name),
        }))
    }
}

/// A greeter that returns UNAVAILABLE for the first N calls, then succeeds.
pub(crate) struct FailFirstNGreeter {
    msg: String,
    call_count: std::sync::atomic::AtomicU32,
    fail_first_n: u32,
}

impl FailFirstNGreeter {
    pub(crate) fn new(msg: &str, fail_first_n: u32) -> Self {
        Self {
            msg: msg.to_string(),
            call_count: std::sync::atomic::AtomicU32::new(0),
            fail_first_n,
        }
    }
}

#[tonic::async_trait]
impl Greeter for FailFirstNGreeter {
    async fn say_hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count < self.fail_first_n {
            return Err(Status::unavailable("temporarily unavailable"));
        }
        Ok(Response::new(HelloReply {
            message: format!("{}: {}", self.msg, req.into_inner().name),
        }))
    }
}

/// A test server that runs a gRPC service and provides a channel for clients to connect.
pub(crate) struct TestServer {
    /// The gRPC channel for talking to the test server.
    pub channel: Channel,
    /// Signal the server to shutdown.
    pub shutdown: oneshot::Sender<()>,
    /// Handle to wait for server to exit.
    pub handle: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    /// Server address.
    pub addr: SocketAddr,
}

impl NamedService for TestServer {
    const NAME: &'static str = "TestServer";
}

/// Spawns a gRPC greeter server for testing purposes.
pub(crate) async fn spawn_greeter_server(
    msg: &str,
    server_tls: Option<ServerTlsConfig>,
    client_tls: Option<ClientTlsConfig>,
) -> Result<TestServer, Box<dyn Error>> {
    // Bind to an ephemeral port (random free port assigned by OS)
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let (tx, rx) = oneshot::channel();

    let svc = GreeterServer::new(MyGreeter {
        msg: msg.to_string(),
    });

    let handle = tokio::spawn(async move {
        let mut builder = if let Some(tls) = server_tls {
            Server::builder().tls_config(tls)?
        } else {
            Server::builder()
        };
        let res = builder
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await;
        match res {
            Ok(_) => println!("Server exited cleanly"),
            Err(e) => eprintln!("Server error: {e}"),
        }

        Ok(())
    });

    let channel = if let Some(client_tls) = client_tls {
        let endpoint_str = format!("https://{addr}");
        Endpoint::from_shared(endpoint_str)?
            .tls_config(client_tls)?
            .connect()
            .await?
    } else {
        let endpoint_str = format!("http://{addr}");
        Endpoint::from_shared(endpoint_str)?.connect().await?
    };

    Ok(TestServer {
        channel,
        shutdown: tx,
        handle,
        addr,
    })
}

/// Spawns a greeter server that fails the first N requests with UNAVAILABLE.
pub(crate) async fn spawn_fail_first_n_server(
    msg: &str,
    fail_first_n: u32,
) -> Result<TestServer, Box<dyn Error>> {
    spawn_plaintext_server(FailFirstNGreeter::new(msg, fail_first_n)).await
}

/// A greeter whose behavior is scripted by the request name: it stalls for
/// `delay` before answering [`ScriptedGreeter::SLOW`], fails
/// [`ScriptedGreeter::FAIL`] with UNAVAILABLE, and answers all others
/// immediately.
pub(crate) struct ScriptedGreeter {
    msg: String,
    delay: std::time::Duration,
}

impl ScriptedGreeter {
    /// The request name that makes the greeter stall.
    pub(crate) const SLOW: &'static str = "slow";
    /// The request name that makes the greeter fail with UNAVAILABLE.
    pub(crate) const FAIL: &'static str = "fail";
}

#[tonic::async_trait]
impl Greeter for ScriptedGreeter {
    async fn say_hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {
        let name = req.into_inner().name;
        match name.as_str() {
            Self::SLOW => tokio::time::sleep(self.delay).await,
            Self::FAIL => return Err(Status::unavailable("scripted failure")),
            _ => {}
        }
        Ok(Response::new(HelloReply {
            message: format!("{}: {name}", self.msg),
        }))
    }
}

/// Spawns a [`ScriptedGreeter`] server that stalls slow requests for `delay`.
pub(crate) async fn spawn_scripted_server(
    msg: &str,
    delay: std::time::Duration,
) -> Result<TestServer, Box<dyn Error>> {
    spawn_plaintext_server(ScriptedGreeter {
        msg: msg.to_string(),
        delay,
    })
    .await
}

/// Spawns a plaintext gRPC server for `greeter` on an ephemeral port.
async fn spawn_plaintext_server<G: Greeter>(greeter: G) -> Result<TestServer, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let (tx, rx) = oneshot::channel();
    let svc = GreeterServer::new(greeter);

    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await
    });

    let channel = Endpoint::from_shared(format!("http://{addr}"))?
        .connect()
        .await?;

    Ok(TestServer {
        channel,
        shutdown: tx,
        handle,
        addr,
    })
}
