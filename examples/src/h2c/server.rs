use tonic::{transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};
use tower::make::Shared;

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
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    println!("GreeterServer listening on {}", addr);

    let svc = Server::builder()
        .add_service(GreeterServer::new(greeter))
        .into_router();

    let h2c = h2c::H2c { s: svc };

    let server = hyper::Server::bind(&addr).serve(Shared::new(h2c));
    server.await.unwrap();

    Ok(())
}

mod h2c {
    use std::pin::Pin;

    use http::{Request, Response};
    use hyper::body::Incoming;
    use hyper_util::{rt::TokioExecutor, service::TowerToHyperService};
    use tonic::{body::empty_body, transport::AxumBoxBody};
    use tower::Service;

    #[derive(Clone)]
    pub struct H2c<S> {
        pub s: S,
    }

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    impl<S> Service<Request<Incoming>> for H2c<S>
    where
        S: Service<Request<Incoming>, Response = Response<AxumBoxBody>> + Clone + Send + 'static,
        S::Future: Send + 'static,
        S::Error: Into<BoxError> + Sync + Send + 'static,
        S::Response: Send + 'static,
    {
        type Response = hyper::Response<tonic::body::BoxBody>;
        type Error = hyper::Error;
        type Future =
            Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, mut req: hyper::Request<Incoming>) -> Self::Future {
            let svc = self.s.clone();
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
