use crate::{buf::SendBuf, flush::Flush, recv_body::RecvBody};
use futures_util::{future, FutureExt, TryFutureExt};
use h2::{client::SendRequest, RecvStream};
use http::{Request, Response};
use http_body::Body;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_io::{AsyncRead, AsyncWrite};
use tower_service::Service;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub struct Connection<B>
where
    B: Body + Unpin,
    B::Data: Unpin,
{
    client: SendRequest<SendBuf<B::Data>>,
}

impl<B> Connection<B>
where
    B: Body + Send + Unpin + 'static,
    B::Data: Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error>>,
{
    pub async fn handshake<T>(io: T) -> Result<Connection<B>, h2::Error>
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let builder = h2::client::Builder::new();
        let (client, conn) = builder.handshake(io).await?;
        tokio_executor::spawn(conn.map_err(|e| println!("ERROR={}", e)).map(drop));
        Ok(Connection { client })
    }

    pub async fn send(&mut self, request: Request<B>) -> Result<Response<RecvBody>, h2::Error> {
        future::poll_fn(|cx| self.poll_ready(cx)).await?;

        self.call(request).await
    }
}

impl<B> Service<Request<B>> for Connection<B>
where
    B: Body + Send + Unpin + 'static,
    B::Data: Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error>>,
{
    type Response = Response<RecvBody>;
    type Error = h2::Error;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client.poll_ready(cx)
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let (parts, body) = request.into_parts();
        let request = Request::from_parts(parts, ());

        let eos = body.is_end_stream();

        let res = self.client.send_request(request, eos);

        let (response, send_body) = match res {
            Ok(success) => success,
            Err(e) => {
                return Box::pin(future::err(e));
            }
        };

        if !eos {
            let flush = Flush::new(body, send_body);
            tokio_executor::spawn(flush.map(drop));
        }

        Box::pin(response.map_ok(|r| r.map(RecvBody::new)))
    }
}
