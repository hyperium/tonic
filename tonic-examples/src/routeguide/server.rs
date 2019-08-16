#![feature(async_await)]

use futures::Stream;
use tokio::net::TcpListener;
use tonic::{Request, Response, Status};
use tower_h2::Server;

pub mod routeguide {
    include!(concat!(env!("OUT_DIR"), "/routeguide.rs"));
}

use routeguide::*;

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

#[derive(Default, Clone)]
pub struct RouteGuide {
    data: String,
}

#[tonic::server(service = "routeguide.RouteGuide", proto = "routeguide")]
impl RouteGuide {
    pub async fn get_feature(&self, _req: Request<Point>) -> Result<Response<Feature>, Status> {
        unimplemented!()
    }

    pub async fn list_features(
        &self,
        _req: Request<Rectangle>,
    ) -> Result<Response<BoxStream<Feature>>, Status> {
        unimplemented!()
    }

    pub async fn record_route(
        &self,
        _req: Request<impl Stream<Item = Result<Point, Status>>>,
    ) -> Result<Response<RouteSummary>, Status> {
        unimplemented!()
    }

    // pub async fn route_chat(
    //     &self,
    //     _req: Request<impl Stream<Item = Result<RouteNote, Status>>>,
    // ) -> Result<Response<impl Stream<Item = Result<RouteNote, Status>>>, Status> {
    //     unimplemented!()
    // }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let mut bind = TcpListener::bind(&addr)?;

    let route_guide = RouteGuide::default();
    let mut server = Server::new(RouteGuideServer::new(route_guide), Default::default());

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
