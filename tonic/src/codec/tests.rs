// use super::{
//     encode_server,
//     prost::{ProstDecoder, ProstEncoder},
//     Streaming,
// };
// use crate::Status;
// use bytes04 as bytes;
// use bytes04::{Buf, BufMut, Bytes, BytesMut};
// use http_body::Body;
// use prost::Message;
// use std::{
//     io::Cursor,
//     pin::Pin,
//     task::{Context, Poll},
// };

// #[derive(Clone, PartialEq, prost::Message)]
// struct Msg {
//     #[prost(bytes, tag = "1")]
//     data: Vec<u8>,
// }

// #[tokio::test]
// async fn decode() {
//     let decoder = ProstDecoder::<Msg>::default();

//     let data = vec![0u8; 10000];
//     let data_len = data.len();
//     let msg = Msg { data };

//     let mut buf = BytesMut::new();
//     let len = msg.encoded_len();

//     buf.reserve(len + 5);
//     buf.put_u8(0);
//     buf.put_u32_be(len as u32);

//     msg.encode(&mut buf).unwrap();

//     let body = body::MockBody::new(&buf[..], 10005, 0);

//     let mut stream = Streaming::new_request(decoder, body);

//     let mut i = 0usize;
//     while let Some(msg) = stream.message().await.unwrap() {
//         assert_eq!(msg.data.len(), data_len);
//         i += 1;
//     }
//     assert_eq!(i, 1);
// }

// #[tokio::test]
// async fn encode() {
//     let encoder = ProstEncoder::<Msg>::default();

//     let data = Vec::from(&[0u8; 1024][..]);
//     let msg = Msg { data };

//     let messages = std::iter::repeat(Ok::<_, Status>(msg)).take(10000);
//     let source = futures_util::stream::iter(messages);

//     let body = encode_server(encoder, source);

//     futures_util::pin_mut!(body);

//     while let Some(r) = body.next().await {
//         r.unwrap();
//     }
// }

// mod body {
//     use crate::Status;
//     use bytes::Bytes;
//     use http_body::Body;
//     use std::{
//         pin::Pin,
//         task::{Context, Poll},
//     };

//     #[derive(Debug)]
//     pub struct MockBody {
//         data: Bytes,

//         // the size of the partial message to send
//         partial_len: usize,

//         // the number of times we've sent
//         count: usize,
//     }

//     impl MockBody {
//         pub fn new(b: &[u8], partial_len: usize, count) -> Self {
//             MockBody {
//                 data: Bytes::copy_from_slice(&b[..]),
//                 partial_len,
//                 count
//             }
//         }
//     }

//     impl Body for MockBody {
//         type Data = Bytes;
//         type Error = Status;

//         fn poll_data(
//             mut self: Pin<&mut Self>,
//             cx: &mut Context<'_>,
//         ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
//             // every other call to poll_data returns data
//             let should_send = self.count % 2 == 0;
//             let data_len = self.data.len();
//             let partial_len = self.partial_len;
//             let count = self.count;
//             if data_len > 0 {
//                 let result = if should_send {
//                     let response =
//                         self.data
//                             .split_to(if count == 0 { partial_len } else { data_len });
//                     Poll::Ready(Some(Ok(response)))
//                 } else {
//                     cx.waker().wake_by_ref();
//                     Poll::Pending
//                 };
//                 // make some fake progress
//                 self.count += 1;
//                 result
//             } else {
//                 Poll::Ready(None)
//             }
//         }

//         fn poll_trailers(
//             self: Pin<&mut Self>,
//             cx: &mut Context<'_>,
//         ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
//             drop(cx);
//             Poll::Ready(Ok(None))
//         }
//     }
// }
