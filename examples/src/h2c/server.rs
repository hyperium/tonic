use std::net::SocketAddr;

use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tonic::{service::Routes, Request, Response, Status};

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
        println!("Got a request from {:?}", request.remote_addr());

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    println!("GreeterServer listening on {}", addr);

    let incoming = TcpListener::bind(addr).await?;
    let svc = Routes::new(GreeterServer::new(greeter));

    let h2c = h2c::H2c { s: svc };

    loop {
        match incoming.accept().await {
            Ok((io, _)) => {
                let router = h2c.clone();
                tokio::spawn(async move {
                    let builder = Builder::new(TokioExecutor::new());
                    let conn = builder.serve_connection_with_upgrades(
                        TokioIo::new(io),
                        TowerToHyperService::new(router),
                    );
                    let _ = conn.await;
                });
            }
            Err(e) => {
                eprintln!("Error accepting connection: {}", e);
            }
        }
    }
}

mod h2c {
    use std::pin::Pin;

    use http::{Request, Response};
    use hyper::body::Incoming;
    use hyper_util::{rt::TokioExecutor, service::TowerToHyperService};
    use tonic::body::{empty_body, BoxBody};
    use tower::{Service, ServiceExt};

    #[derive(Clone)]
    pub struct H2c<S> {
        pub s: S,
    }

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    impl<S> Service<Request<Incoming>> for H2c<S>
    where
        S: Service<Request<BoxBody>, Response = Response<BoxBody>> + Clone + Send + 'static,
        S::Future: Send + 'static,
        S::Error: Into<BoxError> + Sync + Send + 'static,
        S::Response: Send + 'static,
    {
        type Response = hyper::Response<BoxBody>;
        type Error = hyper::Error;
        type Future =
            Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: hyper::Request<Incoming>) -> Self::Future {
            let mut req = req.map(tonic::body::boxed);
            let svc = self
                .s
                .clone()
                .map_request(|req: Request<_>| req.map(tonic::body::boxed));
            Box::pin(async move {
                tokio::spawn(async move {
                    let upgraded_io = hyper::upgrade::on(&mut req).await.unwrap();

                    hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(upgraded_io, TowerToHyperService::new(svc))
                        .await
                        .unwrap();
                });

                let mut res = hyper::Response::new(empty_body());
                *res.status_mut() = http::StatusCode::SWITCHING_PROTOCOLS;
                res.headers_mut().insert(
                    hyper::header::UPGRADE,
                    http::header::HeaderValue::from_static("h2c"),
                );

                Ok(res)
            })
        }
    }
}
