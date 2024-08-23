//! Example of combining tonic grpc routes with regular axum http routes served
//! on a single tcp port.
//!
//! `GreeterServer` is served and also route `/foo` which returns "bar" for
//! http GET requests.
//!
//! The downside of this approach is the tonic server configuration, e.g.
//! [`tonic::transport::server::Server::http2_keepalive_interval`], cannot
//! be used as we don't use the built in tonic server implementation.
//!
//! To configure these, or apply similar defaults to tonic, replace
//! [`axum::serve`] with your own impl or use a crate that allows this.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: std::net::SocketAddr = "[::1]:50051".parse()?;

    // get grpc routes
    let router = grpc::routes()
        .prepare()
        // convert into an axum router
        .into_axum_router()
        // add additional http routes
        .route("/foo", axum::routing::get(|| async { "bar" }));

    println!("GreeterServer & /foo listening on {addr}");

    // serve grpc & http using axum
    //
    // Note: To configure things like tcp_nodelay implement
    //       own `serve` logic or use another crate.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}

mod grpc {
    use hello_world::greeter_server::{Greeter, GreeterServer};
    use hello_world::{HelloReply, HelloRequest};
    use tonic::{Request, Response, Status};

    pub mod hello_world {
        tonic::include_proto!("helloworld");
    }

    pub fn routes() -> tonic::service::Routes {
        tonic::service::Routes::new(GreeterServer::new(MyGreeter::default()))
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
}
