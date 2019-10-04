use super::{prost::{ProstDecoder, ProstEncoder}, Streaming, encode_server};
use crate::Status;
use bytes::{Bytes, Buf, IntoBuf, BytesMut, BufMut};
use prost::Message;
use http_body::Body;
use std::{
    pin::Pin,
    task::{Context, Poll},
    io::Cursor,
};

#[derive(Clone, PartialEq, prost::Message)]
struct Msg {
    #[prost(bytes, tag = "1")]
    data: Vec<u8>,
}

#[tokio::test]
async fn decode() {
    let decoder = ProstDecoder::<Msg>::default();

    let data = Vec::from(&[0u8; 1024][..]);
    let msg = Msg { data };

    let mut buf = BytesMut::new();
    let len = msg.encoded_len();

    buf.reserve(len + 5);
    buf.put_u8(0);
    buf.put_u32_be(len as u32);
    msg.encode(&mut buf).unwrap();

    let body = MockBody(buf.freeze(), 0, 100);

    let mut stream = Streaming::new_request(decoder, body);

    while let Some(_) = stream.message().await.unwrap() {}
}

#[tokio::test]
async fn encode() {
    let encoder = ProstEncoder::<Msg>::default();

    let data = Vec::from(&[0u8; 1024][..]);
    let msg = Msg { data };

    let messages = std::iter::repeat(Ok::<_, Status>(msg)).take(10000);
    let source = futures_util::stream::iter(messages);

    let body = encode_server(encoder, source);

    futures_util::pin_mut!(body);

    while let Some(r) = body.next().await {
        r.unwrap();
    }
}

#[derive(Debug)]
struct MockBody(Bytes, usize, usize);

impl Body for MockBody {
    type Data = Data;
    type Error = Status;

    fn poll_data(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        if self.1 > self.2 {
            self.1 += 1;
            let data = Data(self.0.clone().into_buf());
            Poll::Ready(Some(Ok(data)))
        } else {
            Poll::Ready(None)
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        drop(cx);
        Poll::Ready(Ok(None))
    }
}

struct Data(Cursor<Bytes>);

impl Into<Bytes> for Data {
    fn into(self) -> Bytes {
        self.0.into_inner()
    }
}

impl Buf for Data {
    fn remaining(&self) -> usize {
        self.0.remaining()
    }

    fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }

    fn advance(&mut self, cnt: usize) {
        self.0.advance(cnt)
    }
}
