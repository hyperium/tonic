#![feature(async_await)]

use http::Request;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::net::TcpStream;
use tower_h2::Connection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:8888".parse()?;
    let io = TcpStream::connect(&addr).await?;

    let mut svc = Connection::handshake(io).await?;

    let req = Request::get(format!("http://{}", addr)).body(Body::from(Vec::new()))?;
    let res = svc.send(req).await?;

    println!("RESPONSE={:?}", res);

    Ok(())
}

#[derive(Debug, Default, Clone)]
struct Body(Vec<u8>);

impl From<Vec<u8>> for Body {
    fn from(t: Vec<u8>) -> Self {
        Body(t)
    }
}

impl http_body::Body for Body {
    type Data = std::io::Cursor<Vec<u8>>;
    type Error = std::io::Error;

    fn poll_data(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        if self.0.is_empty() {
            return None.into();
        }

        use std::{io, mem};

        let bytes = mem::replace(&mut self.0, Default::default());
        let buf = io::Cursor::new(bytes);

        Some(Ok(buf)).into()
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        Ok(None).into()
    }
}
