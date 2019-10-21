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

    // encode a few messages
    let nmessages = 3;
    buf.reserve(nmessages * (len + 5));

    for _ in 0..nmessages {
        buf.put_u8(0);
        buf.put_u32_be(len as u32);
        msg.encode(&mut buf).unwrap();
    }

    let body = MockBody {
        data: buf.freeze(),
        partial_len: 10010,
    };

    let mut stream = Streaming::new_request(decoder, body);

    let mut i = 0usize;
    println!("");
    while let Some(msg) = stream.message().await.unwrap() {
        assert_eq!(msg.data.len(), data_len);
        i += 1;
    }
    assert_eq!(i, nmessages);
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
    partial_len: usize,
}

impl Body for MockBody {
    type Data = Data;
    type Error = Status;

    fn poll_data(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let partial_len = self.partial_len;
        let data_len = self.data.len();
        let bytes_to_read = partial_len.min(data_len);
        println!("");
        if bytes_to_read > 0 {
            let response = self.data.split_to(bytes_to_read).into_buf();
            Poll::Ready(Some(Ok(Data(response))))
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
