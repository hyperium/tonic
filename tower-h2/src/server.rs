use crate::{buf::SendBuf, flush::Flush, recv_body::RecvBody};
use futures_util::{future, StreamExt};
use http::{Request, Response};
use http_body::Body;
use std::marker::PhantomData;
use tokio_io::{AsyncRead, AsyncWrite};
use tower_service::Service;
use tower_util::MakeService;

pub struct Server<M, B>
where
    M: MakeService<(), Request<RecvBody>>,
    B: Body,
{
    maker: M,
    builder: h2::server::Builder,
    _pd: PhantomData<B>,
}

impl<M, B> Server<M, B>
where
    M: MakeService<(), Request<RecvBody>, Response = Response<B>>,
    M::MakeError: Into<Box<dyn std::error::Error>>,
    M::Error: Into<Box<dyn std::error::Error>>,
    B: Body + Send + Unpin + 'static,
    B::Data: Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error>>,
{
    pub fn new(maker: M, builder: h2::server::Builder) -> Self {
        Self {
            maker,
            builder,
            _pd: PhantomData
        }
    }

    pub async fn serve<I>(&mut self, io: I) -> Result<(), h2::Error>
    where
        I: AsyncRead + AsyncWrite + Unpin,
    {
        future::poll_fn(|cx| self.maker.poll_ready(cx))
            .await
            .map_err(Into::into)
            .unwrap();
        let mut service = self
            .maker
            .make_service(())
            .await
            .map_err(Into::into)
            .unwrap();

        let mut connection: h2::server::Connection<I, SendBuf<B::Data>> =
            self.builder.handshake(io).await?;

        // TODO: do we want to spawn the connectioons o it can poll_close?

        while let Some(request) = connection.next().await {
            match request {
                Ok((request, send_response)) => {
                    let request = request.map(RecvBody::new);

                    future::poll_fn(|cx| service.poll_ready(cx))
                        .await
                        .map_err(Into::into)
                        .unwrap();

                    // TODO: on error send reset
                    let response = service.call(request).await.map_err(Into::into).unwrap();

                    let fut = handle_request(response, send_response);
                    tokio_executor::spawn(fut);
                }
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }
}

pub async fn handle_request<B>(
        response: Response<B>,
        mut send_response: h2::server::SendResponse<SendBuf<B::Data>>,
    ) where
        B: Body + Send + Unpin + 'static,
        B::Data: Unpin,
        B::Error: Into<Box<dyn std::error::Error>>,
    {
        let (parts, body) = response.into_parts();

        // Check if the response is imemdiately an end-of-stream.
        let eos = body.is_end_stream();

        let response = Response::from_parts(parts, ());

        match send_response.send_response(response, eos) {
            Ok(sr) => {
                if eos {
                    return;
                }

                Flush::new(body, sr).await.unwrap();
            }
            Err(e) => {
                println!("h2 server ERROR={}", e);
            }
        }
    }
