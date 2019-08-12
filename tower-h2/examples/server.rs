#![feature(async_await)]

use futures_util::future;
use http::{Request, Response};
use std::task::{Context, Poll};
use tokio::net::TcpListener;
use tokio_buf::BufStream;
use tower_h2::{RecvBody, Server};
use tower_service::Service;

const ROOT: &'static str = "/";

#[derive(Debug)]
pub struct Svc;

impl Service<Request<RecvBody>> for Svc {
    type Response = Response<Body>;
    type Error = h2::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request<RecvBody>) -> Self::Future {
        let mut rsp = Response::builder();
        rsp.version(http::Version::HTTP_2);

        let uri = req.uri();
        if uri.path() != ROOT {
            let body = Body::from(Vec::new());
            let rsp = rsp.status(404).body(body).unwrap();
            return future::ok(rsp);
        }

        let body = Body::from(Vec::from(&b"heyo!"[..]));
        let rsp = rsp.status(200).body(body).unwrap();
        future::ok(rsp)
    }
}

pub struct MakeSvc;

impl Service<()> for MakeSvc {
    type Response = Svc;
    type Error = std::io::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, _: ()) -> Self::Future {
        future::ok(Svc)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:8888".parse().unwrap();
    let mut bind = TcpListener::bind(&addr)?;

    let mut server = Server::new(MakeSvc, Default::default());

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

#[derive(Debug, Default, Clone)]
pub struct Body(Vec<u8>);

impl From<Vec<u8>> for Body {
    fn from(t: Vec<u8>) -> Self {
        Body(t)
    }
}

impl BufStream for Body {
    type Item = std::io::Cursor<Vec<u8>>;
    type Error = std::io::Error;

    fn poll_buf(&mut self, _cx: &mut Context<'_>) -> Poll<Option<Result<Self::Item, Self::Error>>> {
        if self.0.is_empty() {
            return None.into();
        }

        use std::{io, mem};

        let bytes = mem::replace(&mut self.0, Default::default());
        let buf = io::Cursor::new(bytes);

        Some(Ok(buf)).into()
    }
}
