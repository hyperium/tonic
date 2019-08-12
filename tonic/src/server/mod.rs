use crate::{Request, Response, Status};
use async_stream::stream;
use bytes::{Bytes, BytesMut, IntoBuf};
use futures_core::TryStream;
use futures_util::{stream, StreamExt, TryStreamExt};
use std::future::Future;
use tokio_codec::{Decoder, Encoder};
use tower_service::Service;

#[allow(dead_code)]
type Result<T> = std::result::Result<Response<T>, Status>;

pub trait Codec {
    type Encode;
    type Decode;
}

pub struct Encode<T, U> {
    inner: T,
    source: U,
}

impl<T, U> Encode<T, U>
where
    T: Encoder,
    U: TryStream<Ok = T::Item, Error = Status> + Unpin,
{
    pub fn new(inner: T, source: U) -> Self {
        Encode { inner, source }
    }

    pub fn encode<'a>(
        &'a mut self,
        buf: &'a mut BytesMut,
    ) -> impl TryStream<Ok = crate::body::BytesBuf, Error = Status> + 'a {
        stream! {
            loop {
                match self.source.try_next().await {
                    Ok(Some(item)) => {
                        self.inner.encode(item, buf).map_err(drop).unwrap();
                        let len = buf.len();
                        yield Ok(buf.split_to(len).freeze().into_buf());
                    },
                    Ok(None) => break,
                    Err(status) => yield Err(status),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::body::AsyncBody;
    use crate::server::Encode;
    use bytes::Bytes;
    use tokio_codec::BytesCodec;

    #[test]
    fn body() {
        let stream = futures_util::stream::iter(vec![Ok(Bytes::new())]);
        let encode = Encode::new(BytesCodec::new(), stream);
    }
}

// impl<T, U> http_body::Body for Encode<T, U>

// pub struct Grpc<T> {
//     opdec: T,
// }

// impl<T: Codec> Grpc<T> {
//     pub async fn unary<B>(&mut self, message: B) -> Result<Response<B>> {
//         self.server_streaming(stream::once(message)).await
//     }

//     pub async fn server_streaming<B>(
//         &mut self,
//         stream: impl Stream,
//     ) -> Result<Response<impl http_body::Body>> {
//         unimplemetned!()
//     }

//     fn map_request<B>(&mut self, request: http::Request<B>) -> Request<B> {
//         Request::from_http(request)
//     }
// }
