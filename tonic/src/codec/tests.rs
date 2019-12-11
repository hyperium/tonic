use super::{encode_server, Decoder, Encoder, Streaming};
use crate::Status;
use bytes::{Buf, BufMut, BytesMut};
use http_body::Body;

const LEN: usize = 10000;

#[tokio::test]
async fn decode() {
    let decoder = MockDecoder::default();

    let msg = vec![0u8; LEN];

    let mut buf = BytesMut::new();

    buf.reserve(msg.len() + 5);
    buf.put_u8(0);
    buf.put_u32(msg.len() as u32);

    buf.put(&msg[..]);

    let body = body::MockBody::new(&buf[..], 10005, 0);

    let mut stream = Streaming::new_request(decoder, body);

    let mut i = 0usize;
    while let Some(output_msg) = stream.message().await.unwrap() {
        assert_eq!(output_msg.len(), msg.len());
        i += 1;
    }
    assert_eq!(i, 1);
}

#[tokio::test]
async fn encode() {
    let encoder = MockEncoder::default();

    let msg = Vec::from(&[0u8; 1024][..]);

    let messages = std::iter::repeat(Ok::<_, Status>(msg)).take(10000);
    let source = futures_util::stream::iter(messages);

    let body = encode_server(encoder, source);

    futures_util::pin_mut!(body);

    while let Some(r) = body.data().await {
        r.unwrap();
    }
}

#[derive(Debug, Clone, Default)]
struct MockEncoder;

impl Encoder for MockEncoder {
    type Item = Vec<u8>;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
        buf.put(&item[..]);
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
struct MockDecoder;

impl Decoder for MockDecoder {
    type Item = Vec<u8>;
    type Error = Status;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let out = Vec::from(&buf[..LEN]);
        buf.advance(LEN);
        Ok(Some(out))
    }
}

mod body {
    use crate::Status;
    use bytes::Bytes;
    use http_body::Body;
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    #[derive(Debug)]
    pub(super) struct MockBody {
        data: Bytes,

        // the size of the partial message to send
        partial_len: usize,

        // the number of times we've sent
        count: usize,
    }

    impl MockBody {
        pub(super) fn new(b: &[u8], partial_len: usize, count: usize) -> Self {
            MockBody {
                data: Bytes::copy_from_slice(&b[..]),
                partial_len,
                count,
            }
        }
    }

    impl Body for MockBody {
        type Data = Bytes;
        type Error = Status;

        fn poll_data(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
            // every other call to poll_data returns data
            let should_send = self.count % 2 == 0;
            let data_len = self.data.len();
            let partial_len = self.partial_len;
            let count = self.count;
            if data_len > 0 {
                let result = if should_send {
                    let response =
                        self.data
                            .split_to(if count == 0 { partial_len } else { data_len });
                    Poll::Ready(Some(Ok(response)))
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                };
                // make some fake progress
                self.count += 1;
                result
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
}
