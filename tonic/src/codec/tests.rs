use super::{
    encode_server,
    prost::{ProstDecoder, ProstEncoder},
    Streaming,
};
use crate::Status;
use bytes::{Buf, BufMut, Bytes, BytesMut, IntoBuf};
use http_body::Body;
use prost::Message;
use std::{
    io::Cursor,
    pin::Pin,
    task::{Context, Poll},
};

#[derive(Clone, PartialEq, prost::Message)]
struct Msg {
    #[prost(bytes, tag = "1")]
    data: Vec<u8>,
}

#[tokio::test]
async fn decode() {
    let decoder = ProstDecoder::<Msg>::default();

    let data = Vec::from(&[0u8; 10000][..]);
    let data_len = data.len();
    let msg = Msg { data };

    let mut buf = BytesMut::new();
    let len = msg.encoded_len();

    buf.reserve(len + 5);
    buf.put_u8(0);
    buf.put_u32_be(len as u32);
    msg.encode(&mut buf).unwrap();

    let upper = 100;
    let body = MockBody {
        data: buf.freeze(),
        lower: 0,
        upper: upper,
    };

    let mut stream = Streaming::new_request(decoder, body);

    let mut i = 0usize;
    while let Some(msg) = stream.message().await.unwrap() {
        assert_eq!(msg.data.len(), data_len);
        i += 1;
    }
    assert_eq!(i, upper);
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
struct MockBody {
    data: Bytes,
    lower: usize,
    upper: usize,
}

impl Body for MockBody {
    type Data = Data;
    type Error = Status;

    fn poll_data(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        if self.upper > self.lower {
            self.lower += 1;
            let data = Data(self.data.clone().into_buf());
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
