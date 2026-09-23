/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::error::Error as StdError;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use bytes::Buf;
use bytes::Bytes;
use http::Response;
use http_body::Body;
use http_body::Frame;
use pin_project_lite::pin_project;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_stream::Stream;
use tonic::Code;
use tonic::Status as TonicStatus;
use tonic::codec::EncodeBody;
use tonic::codec::SingleMessageCompressionOverride;
use tonic::codec::Streaming;

use super::validation::is_reserved_header;
use crate::codec::BufEncoder;
use crate::codec::BytesDecoder;
use crate::core::RecvMessage;
use crate::metadata::KeyAndValueRef;
use crate::rt::BoxedTaskHandle;
use crate::server::RecvStream;
use crate::server::ResponseHeaders;
use crate::server::ResponseStreamItem;
use crate::server::SendOptions;
use crate::server::SendStream;
use crate::server::Trailers;
use crate::status::StatusCodeError;

/// The transport-level [`RecvStream`] for the Hyper transport.
///
/// Deframes Length-Prefixed Message (LPM) payloads from an incoming HTTP/2
/// request body and decodes each payload into a [`RecvMessage`].
pub struct HyperServerRecvStream {
    /// Stream of deframed gRPC message payloads (with the 5-byte LPM header stripped).
    inner: Streaming<Bytes>,
}

impl HyperServerRecvStream {
    /// Creates a new [`HyperServerRecvStream`] wrapping an HTTP request body.
    pub fn new<B>(body: B, max_message_size: Option<usize>) -> Self
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
    {
        let streaming = Streaming::new_request(BytesDecoder, body, None, max_message_size);
        Self { inner: streaming }
    }
}

impl RecvStream for HyperServerRecvStream {
    /// Deframes the next message payload from the request body and decodes it into `msg`.
    ///
    /// This implementation does not fuse the stream after returning `None` or
    /// `Some(Err(()))`; callers must uphold the [`RecvStream`] contract and not
    /// call `next` again after reaching a terminal state.
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
        match self.inner.message().await {
            Ok(Some(mut bytes)) => Some(msg.decode(&mut bytes).map_err(|_| ())),
            Ok(None) => None, // Body exhausted.
            Err(_) => Some(Err(())),
        }
    }
}

/// The initial HTTP/2 response frame produced by a handler task.
pub enum InitialResponse {
    /// Initial [`ResponseHeaders`] were sent; response body will stream via [`EncodeBody`].
    Headers(ResponseHeaders),
    /// Handler finished without sending initial headers; response is HTTP/2 `Trailers-Only`.
    TrailersOnly(Trailers),
}

/// The transport-level [`SendStream`] for the Hyper transport.
///
/// - `Headers`: Sends `ResponseHeaders` via a oneshot channel to the connection
///   service, which uses them to build the initial HTTP/2 response headers.
/// - `Message`: Encodes and pushes payload buffers via an mpsc channel to
///   `tonic::codec::EncodeBody`, which frames them with LPM headers and yields
///   them as HTTP/2 DATA frames.
pub struct HyperServerSendStream {
    initial_tx: Option<oneshot::Sender<InitialResponse>>,
    /// `Ok` carries an encoded message; `Err` carries the final [`Trailers`],
    /// which are emitted as the trailing HTTP/2 `HEADERS` frame.
    body_tx: mpsc::Sender<Result<Box<dyn Buf + Send + Sync>, Trailers>>,
}

impl HyperServerSendStream {
    /// Creates a new [`HyperServerSendStream`] awaiting initial response headers.
    pub fn new(
        initial_tx: oneshot::Sender<InitialResponse>,
        body_tx: mpsc::Sender<Result<Box<dyn Buf + Send + Sync>, Trailers>>,
    ) -> Self {
        Self {
            initial_tx: Some(initial_tx),
            body_tx,
        }
    }

    /// Consumes the send stream and dispatches the final [`Trailers`] to either
    /// the initial `Trailers-Only` response channel (if no headers were sent)
    /// or the streaming [`EncodeBody`] channel (if headers were already sent).
    pub async fn send_trailers(self, trailers: Trailers) {
        if let Some(initial_tx) = self.initial_tx {
            let _ = initial_tx.send(InitialResponse::TrailersOnly(trailers));
        } else if trailers.status().is_err() || !trailers.metadata().is_empty() {
            let _ = self.body_tx.send(Err(trailers)).await;
        }
    }
}

impl SendStream for HyperServerSendStream {
    /// Sends response headers or an encoded message payload on the stream.
    ///
    /// This implementation does not fuse the stream after returning `Err(())`;
    /// callers must uphold the [`SendStream`] contract and not call `send`
    /// again after an error.
    async fn send(
        &mut self,
        item: ResponseStreamItem<'_>,
        _options: SendOptions,
    ) -> Result<(), ()> {
        match item {
            ResponseStreamItem::Headers(h) => {
                let tx = self.initial_tx.take().ok_or(())?;
                tx.send(InitialResponse::Headers(h)).map_err(|_| ())
            }
            ResponseStreamItem::Message(msg) => {
                let buf = msg.encode().map_err(|_| ())?;

                // TODO: Reconsider requiring explicit Headers before Message once all callers
                // (or a server-side SendStreamValidator) guarantee initial headers are sent first.
                if let Some(tx) = self.initial_tx.take() {
                    let _ = tx.send(InitialResponse::Headers(ResponseHeaders::default()));
                }

                self.body_tx.send(Ok(buf)).await.map_err(|_| ())
            }
        }
    }
}

/// RAII guard that aborts a spawned [`BoxedTaskHandle`] when dropped.
///
/// Stored inside [`ResponseMessageStream`] from the moment the handler task is
/// spawned and moved into the streaming [`EncodeBody`] once initial
/// [`ResponseHeaders`] are received, ensuring that `RST_STREAM` or connection
/// teardown aborts the spawned `dyn_handle` task immediately.
struct AbortTaskOnDrop(BoxedTaskHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Adapts the handler's response channel into the `Result<_, TonicStatus>`
/// stream required by [`EncodeBody`], converting the final [`Trailers`] into a
/// [`TonicStatus`].
///
/// Also keeps an [`AbortTaskOnDrop`] guard alive for as long as Hyper holds the
/// response body.
pub struct ResponseMessageStream {
    body_rx: mpsc::Receiver<Result<Box<dyn Buf + Send + Sync>, Trailers>>,
    _task_guard: AbortTaskOnDrop,
}

impl Stream for ResponseMessageStream {
    type Item = Result<Box<dyn Buf + Send + Sync>, TonicStatus>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.body_rx
            .poll_recv(cx)
            .map(|item| item.map(|res| res.map_err(tonic_status_from_trailers)))
    }
}

pin_project! {
    /// HTTP/2 response body returned by the Hyper server transport.
    #[project = ServerResponseBodyProj]
    #[derive(Default)]
    pub enum ServerResponseBody {
        /// Empty body used for HTTP/2 `Trailers-Only` responses (where `grpc-status`
        /// is sent in the initial `HEADERS` frame with `END_STREAM` set).
        #[default]
        Empty,
        /// LPM-framed gRPC message stream followed by a trailing `HEADERS` frame.
        Streaming {
            #[pin]
            body: EncodeBody<BufEncoder, ResponseMessageStream>,
        },
    }
}

impl ServerResponseBody {
    /// Creates a streaming body that LPM-frames messages received on `body_rx`
    /// and emits the final [`Trailers`] as a trailing `HEADERS` frame.
    ///
    /// `task_handle` is aborted when the body is dropped.
    pub fn streaming(
        body_rx: mpsc::Receiver<Result<Box<dyn Buf + Send + Sync>, Trailers>>,
        task_handle: BoxedTaskHandle,
    ) -> Self {
        let messages = ResponseMessageStream {
            body_rx,
            _task_guard: AbortTaskOnDrop(task_handle),
        };
        let body = EncodeBody::new_server(
            BufEncoder,
            messages,
            None,
            SingleMessageCompressionOverride::default(),
            None,
        );
        Self::Streaming { body }
    }
}

impl Body for ServerResponseBody {
    type Data = Bytes;
    type Error = TonicStatus;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.project() {
            ServerResponseBodyProj::Empty => Poll::Ready(None),
            ServerResponseBodyProj::Streaming { body } => body.poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Streaming { body } => body.is_end_stream(),
        }
    }
}

/// Builds an HTTP/2 `Trailers-Only` [`Response`]: an empty body with the gRPC
/// status and trailing metadata in the initial `HEADERS` frame.
// TODO: Consider serializing `grpc-status`, `grpc-message`, and trailing metadata
// directly into the `Response` instead of using `TonicStatus::into_http` as a middleman.
pub fn trailers_only_response<B: Default>(trailers: Trailers) -> Response<B> {
    tonic_status_from_trailers(trailers).into_http()
}

/// Converts a [`StatusCodeError`] into the corresponding [`Code`].
fn tonic_code_from_status_code(code: StatusCodeError) -> Code {
    match code {
        StatusCodeError::Cancelled => Code::Cancelled,
        StatusCodeError::Unknown => Code::Unknown,
        StatusCodeError::InvalidArgument => Code::InvalidArgument,
        StatusCodeError::DeadlineExceeded => Code::DeadlineExceeded,
        StatusCodeError::NotFound => Code::NotFound,
        StatusCodeError::AlreadyExists => Code::AlreadyExists,
        StatusCodeError::PermissionDenied => Code::PermissionDenied,
        StatusCodeError::ResourceExhausted => Code::ResourceExhausted,
        StatusCodeError::FailedPrecondition => Code::FailedPrecondition,
        StatusCodeError::Aborted => Code::Aborted,
        StatusCodeError::OutOfRange => Code::OutOfRange,
        StatusCodeError::Unimplemented => Code::Unimplemented,
        StatusCodeError::Internal => Code::Internal,
        StatusCodeError::Unavailable => Code::Unavailable,
        StatusCodeError::DataLoss => Code::DataLoss,
        StatusCodeError::Unauthenticated => Code::Unauthenticated,
    }
}

/// Converts server [`Trailers`] into a [`TonicStatus`], stripping reserved
/// HTTP/2 and gRPC headers from the trailing metadata.
fn tonic_status_from_trailers(mut trailers: Trailers) -> TonicStatus {
    trailers.metadata_mut().retain(|entry| match entry {
        KeyAndValueRef::Ascii(key, _) => !is_reserved_header(key.as_str()),
        KeyAndValueRef::Binary(key, _) => !is_reserved_header(key.as_str()),
    });
    let (status, metadata) = trailers.into_parts();
    let mut tonic_status = match status {
        Ok(()) => TonicStatus::ok(""),
        Err(err) => {
            let (code, message) = err.into_parts();
            TonicStatus::new(tonic_code_from_status_code(code), message)
        }
    };
    if !metadata.is_empty() {
        *tonic_status.metadata_mut() = metadata.into();
    }
    tonic_status
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use bytes::BytesMut;
    use http_body::Frame;
    use http_body_util::BodyExt;
    use http_body_util::StreamBody;
    use tokio_stream::empty;
    use tokio_stream::iter;

    use super::*;
    use crate::core::SendMessage;
    use crate::metadata::MetadataMap;
    use crate::metadata::MetadataValue;
    use crate::rt::TaskHandle;
    use crate::server::transport::hyper::test::BytesCapture;
    use crate::server::transport::hyper::test::TestSendMessage;
    use crate::status::StatusError;

    struct TrackingTaskHandle(Arc<AtomicBool>);

    impl TaskHandle for TrackingTaskHandle {
        fn abort(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct FailingRecvMessage;

    impl RecvMessage for FailingRecvMessage {
        fn decode(&mut self, _data: &mut dyn Buf) -> Result<(), String> {
            Err("simulated decode failure".to_string())
        }
    }

    struct FailingSendMessage;

    impl SendMessage for FailingSendMessage {
        fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
            Err("simulated encode failure".to_string())
        }
    }

    fn make_lpm_frame(payload: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(5 + payload.len());
        buf.extend_from_slice(&[0u8]); // uncompressed
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(payload);
        buf.freeze()
    }

    #[tokio::test]
    async fn recv_stream_yields_multiple_messages_then_none() {
        let lpm1 = make_lpm_frame(b"one");
        let lpm2 = make_lpm_frame(b"two");
        let body = StreamBody::new(iter(vec![
            Ok::<_, Infallible>(Frame::data(lpm1)),
            Ok::<_, Infallible>(Frame::data(lpm2)),
        ]));
        let mut recv = HyperServerRecvStream::new(body, None);

        let mut msg1 = BytesCapture::new();
        let result1 = recv.next(&mut msg1).await;
        assert!(matches!(result1, Some(Ok(()))));
        assert_eq!(msg1.data(), Some(b"one".as_slice()));

        let mut msg2 = BytesCapture::new();
        let result2 = recv.next(&mut msg2).await;
        assert!(matches!(result2, Some(Ok(()))));
        assert_eq!(msg2.data(), Some(b"two".as_slice()));

        let mut msg3 = BytesCapture::new();
        let result3 = recv.next(&mut msg3).await;
        assert!(result3.is_none());
    }

    #[tokio::test]
    async fn recv_stream_empty_body_returns_none() {
        let body = StreamBody::new(empty::<Result<Frame<Bytes>, Infallible>>());
        let mut recv = HyperServerRecvStream::new(body, None);
        let mut msg = BytesCapture::new();

        let result = recv.next(&mut msg).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn recv_stream_exceeds_max_message_size_returns_error() {
        let lpm = make_lpm_frame(b"too large message");
        let body = StreamBody::new(iter(vec![Ok::<_, Infallible>(Frame::data(lpm))]));
        // Configured max size of 5 bytes < 17 bytes payload
        let mut recv = HyperServerRecvStream::new(body, Some(5));
        let mut msg = BytesCapture::new();

        let result = recv.next(&mut msg).await;
        assert!(matches!(result, Some(Err(()))));
    }

    #[tokio::test]
    async fn recv_stream_decode_error_returns_error() {
        let lpm = make_lpm_frame(b"corrupt-payload");
        let body = StreamBody::new(iter(vec![Ok::<_, Infallible>(Frame::data(lpm))]));
        let mut recv = HyperServerRecvStream::new(body, None);
        let mut msg = FailingRecvMessage;

        let result = recv.next(&mut msg).await;
        assert!(matches!(result, Some(Err(()))));
    }

    #[tokio::test]
    async fn send_stream_sends_headers_then_messages() {
        let (headers_tx, headers_rx) = oneshot::channel();
        let (body_tx, mut body_rx) = mpsc::channel(32);
        let mut tx = HyperServerSendStream::new(headers_tx, body_tx);
        let msg = TestSendMessage::new(Bytes::from_static(b"hello-body"));

        assert!(
            tx.send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default()
            )
            .await
            .is_ok()
        );
        assert!(matches!(
            headers_rx.await.unwrap(),
            InitialResponse::Headers(_)
        ));

        assert!(
            tx.send(ResponseStreamItem::Message(&msg), SendOptions::default())
                .await
                .is_ok()
        );
        let mut sent_buf = body_rx.recv().await.unwrap().unwrap();
        assert_eq!(
            sent_buf.copy_to_bytes(sent_buf.remaining()),
            Bytes::from_static(b"hello-body")
        );
    }

    #[tokio::test]
    async fn send_stream_rejects_duplicate_headers() {
        let (headers_tx, _headers_rx) = oneshot::channel();
        let (body_tx, _body_rx) = mpsc::channel(32);
        let mut tx = HyperServerSendStream::new(headers_tx, body_tx);

        assert!(
            tx.send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default()
            )
            .await
            .is_ok()
        );
        assert!(
            tx.send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn send_stream_message_without_headers_flushes_default_headers() {
        let (headers_tx, headers_rx) = oneshot::channel();
        let (body_tx, mut body_rx) = mpsc::channel(32);
        let mut tx = HyperServerSendStream::new(headers_tx, body_tx);
        let msg = TestSendMessage::new(Bytes::from_static(b"first-msg"));

        assert!(
            tx.send(ResponseStreamItem::Message(&msg), SendOptions::default())
                .await
                .is_ok()
        );
        assert!(matches!(
            headers_rx.await.unwrap(),
            InitialResponse::Headers(_)
        ));
        let mut sent_buf = body_rx.recv().await.unwrap().unwrap();
        assert_eq!(
            sent_buf.copy_to_bytes(sent_buf.remaining()),
            Bytes::from_static(b"first-msg")
        );
    }

    #[tokio::test]
    async fn send_stream_headers_after_message_returns_error() {
        let (headers_tx, _headers_rx) = oneshot::channel();
        let (body_tx, _body_rx) = mpsc::channel(32);
        let mut tx = HyperServerSendStream::new(headers_tx, body_tx);
        let msg = TestSendMessage::new(Bytes::from_static(b"first-msg"));
        tx.send(ResponseStreamItem::Message(&msg), SendOptions::default())
            .await
            .unwrap();

        assert!(
            tx.send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn send_stream_headers_when_initial_receiver_dropped_returns_error() {
        let (headers_tx, headers_rx) = oneshot::channel();
        let (body_tx, _body_rx) = mpsc::channel(32);
        let mut tx = HyperServerSendStream::new(headers_tx, body_tx);

        drop(headers_rx);

        assert!(
            tx.send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn send_stream_message_encode_error_preserves_initial_response_channel() {
        let (headers_tx, headers_rx) = oneshot::channel();
        let (body_tx, mut body_rx) = mpsc::channel(32);
        let mut tx = HyperServerSendStream::new(headers_tx, body_tx);

        assert!(
            tx.send(
                ResponseStreamItem::Message(&FailingSendMessage),
                SendOptions::default()
            )
            .await
            .is_err()
        );

        tx.send_trailers(Trailers::new(Err(StatusError::new(
            StatusCodeError::Internal,
            "encode failed",
        ))))
        .await;
        let initial = headers_rx.await.expect("expected TrailersOnly response");
        let InitialResponse::TrailersOnly(trailers) = initial else {
            panic!("expected TrailersOnly response");
        };
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Internal);
        assert_eq!(err.message(), "encode failed");
        assert!(body_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn send_stream_message_when_body_receiver_dropped_returns_error() {
        let (headers_tx, _headers_rx) = oneshot::channel();
        let (body_tx, body_rx) = mpsc::channel(32);
        let mut tx = HyperServerSendStream::new(headers_tx, body_tx);
        let msg = TestSendMessage::new(Bytes::from_static(b"orphaned-msg"));

        drop(body_rx);

        assert!(
            tx.send(ResponseStreamItem::Message(&msg), SendOptions::default())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn send_trailers_without_headers_sends_trailers_only_initial_response() {
        let (initial_tx, initial_rx) = oneshot::channel();
        let (body_tx, mut body_rx) = mpsc::channel(4);
        let tx = HyperServerSendStream::new(initial_tx, body_tx);

        tx.send_trailers(Trailers::new(Err(StatusError::new(
            StatusCodeError::Unauthenticated,
            "missing token",
        ))))
        .await;

        let initial = initial_rx.await.expect("expected initial response");
        let InitialResponse::TrailersOnly(trailers) = initial else {
            panic!("expected TrailersOnly response");
        };
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Unauthenticated);
        assert_eq!(err.message(), "missing token");
        assert!(body_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn send_trailers_after_headers_with_empty_ok_closes_body_channel() {
        let (initial_tx, initial_rx) = oneshot::channel();
        let (body_tx, mut body_rx) = mpsc::channel(4);
        let mut tx = HyperServerSendStream::new(initial_tx, body_tx);

        tx.send(
            ResponseStreamItem::Headers(ResponseHeaders::default()),
            SendOptions::default(),
        )
        .await
        .unwrap();
        assert!(matches!(
            initial_rx.await.unwrap(),
            InitialResponse::Headers(_)
        ));

        tx.send_trailers(Trailers::new(Ok(()))).await;
        assert!(body_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn send_trailers_after_headers_with_error_sends_trailers_on_body_channel() {
        let (initial_tx, initial_rx) = oneshot::channel();
        let (body_tx, mut body_rx) = mpsc::channel(4);
        let mut tx = HyperServerSendStream::new(initial_tx, body_tx);

        tx.send(
            ResponseStreamItem::Headers(ResponseHeaders::default()),
            SendOptions::default(),
        )
        .await
        .unwrap();
        assert!(matches!(
            initial_rx.await.unwrap(),
            InitialResponse::Headers(_)
        ));

        tx.send_trailers(Trailers::new(Err(StatusError::new(
            StatusCodeError::Internal,
            "mid-stream failure",
        ))))
        .await;
        let item = body_rx.recv().await.expect("expected Err(trailers) item");
        let trailers = item.err().expect("expected Err variant");
        let err = trailers.status().as_ref().unwrap_err();
        assert_eq!(err.code(), StatusCodeError::Internal);
        assert_eq!(err.message(), "mid-stream failure");
        assert!(body_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn send_trailers_after_headers_with_ok_metadata_sends_trailers_on_body_channel() {
        let (initial_tx, initial_rx) = oneshot::channel();
        let (body_tx, mut body_rx) = mpsc::channel(4);
        let mut tx = HyperServerSendStream::new(initial_tx, body_tx);

        tx.send(
            ResponseStreamItem::Headers(ResponseHeaders::default()),
            SendOptions::default(),
        )
        .await
        .unwrap();
        assert!(matches!(
            initial_rx.await.unwrap(),
            InitialResponse::Headers(_)
        ));

        let mut md = MetadataMap::new();
        md.insert("x-trailer-meta", MetadataValue::from_static("present"));
        tx.send_trailers(Trailers::new(Ok(())).with_metadata(md))
            .await;

        let item = body_rx.recv().await.expect("expected Err(trailers) item");
        let delivered = item.err().expect("expected Err variant");
        assert!(delivered.status().is_ok());
        assert_eq!(
            delivered
                .metadata()
                .get("x-trailer-meta")
                .map(|v| v.to_str()),
            Some("present")
        );
        assert!(body_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn server_response_body_empty_reports_end_of_stream() {
        let mut empty_body = pin!(ServerResponseBody::default());
        assert!(empty_body.is_end_stream());
        assert!(empty_body.frame().await.is_none());
    }

    #[tokio::test]
    async fn server_response_body_streaming_yields_frames_and_aborts_task_on_completion() {
        let aborted = Arc::new(AtomicBool::new(false));
        let task_handle: BoxedTaskHandle = Box::new(TrackingTaskHandle(aborted.clone()));
        let (body_tx, body_rx) = mpsc::channel(4);
        body_tx
            .send(Ok(
                Box::new(Bytes::from_static(b"payload")) as Box<dyn Buf + Send + Sync>
            ))
            .await
            .unwrap();

        let mut streaming_body = pin!(ServerResponseBody::streaming(body_rx, task_handle));
        assert!(!streaming_body.is_end_stream());
        assert!(!aborted.load(Ordering::SeqCst));

        let data_frame = streaming_body
            .frame()
            .await
            .expect("expected data frame")
            .expect("data frame should be Ok");
        assert_eq!(
            data_frame.into_data().expect("expected DATA frame"),
            make_lpm_frame(b"payload")
        );
        assert!(!streaming_body.is_end_stream());
        assert!(!aborted.load(Ordering::SeqCst));

        drop(body_tx);

        let trailers_frame = streaming_body
            .frame()
            .await
            .expect("expected trailers frame")
            .expect("trailers frame should be Ok");
        let trailers = trailers_frame
            .into_trailers()
            .expect("expected TRAILERS frame");
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("0")
        );
        assert!(streaming_body.is_end_stream());
        assert!(streaming_body.frame().await.is_none());
        assert!(aborted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn server_response_body_streaming_emits_error_trailers_with_metadata() {
        let task_handle: BoxedTaskHandle =
            Box::new(TrackingTaskHandle(Arc::new(AtomicBool::new(false))));
        let (body_tx, body_rx) = mpsc::channel(4);
        let mut md = MetadataMap::new();
        md.insert("x-trailer-meta", MetadataValue::from_static("present"));
        md.insert("grpc-timeout", MetadataValue::from_static("5S"));
        body_tx
            .send(Err(Trailers::new(Err(StatusError::new(
                StatusCodeError::Unavailable,
                "try later",
            )))
            .with_metadata(md)))
            .await
            .unwrap();
        drop(body_tx);

        let mut streaming_body = pin!(ServerResponseBody::streaming(body_rx, task_handle));
        let trailers = streaming_body
            .frame()
            .await
            .expect("expected trailers frame")
            .expect("trailers frame should be Ok")
            .into_trailers()
            .expect("expected TRAILERS frame");
        assert_eq!(trailers.get("grpc-status").unwrap(), "14");
        assert_eq!(trailers.get("grpc-message").unwrap(), "try%20later");
        assert_eq!(trailers.get("x-trailer-meta").unwrap(), "present");
        assert!(trailers.get("grpc-timeout").is_none());
        assert!(streaming_body.is_end_stream());
    }

    #[test]
    fn trailers_only_response_ok_without_metadata() {
        let response = trailers_only_response::<()>(Trailers::new(Ok(())));
        assert_eq!(response.status(), http::StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers.get("content-type").unwrap(), "application/grpc");
        assert_eq!(headers.get("grpc-status").unwrap(), "0");
        assert!(headers.get("grpc-message").is_none());
    }

    #[test]
    fn trailers_only_response_preserves_custom_metadata_and_strips_reserved_headers() {
        let mut md = MetadataMap::new();
        md.insert("x-custom-trailer", MetadataValue::from_static("val"));
        md.insert_bin(
            "x-custom-bin",
            MetadataValue::from_bytes(&[0xde, 0xad, 0xbe, 0xef]),
        );
        for reserved in [
            "grpc-timeout",
            "grpc-encoding",
            "grpc-accept-encoding",
            "grpc-message-type",
        ] {
            md.insert(reserved, MetadataValue::from_static("reserved"));
        }
        md.insert_bin(
            "grpc-status-details-bin",
            MetadataValue::from_bytes(&[0x00]),
        );
        let trailers = Trailers::new(Err(StatusError::new(
            StatusCodeError::NotFound,
            "no such thing",
        )))
        .with_metadata(md);

        let response = trailers_only_response::<()>(trailers);
        let headers = response.headers();
        assert_eq!(headers.get("grpc-status").unwrap(), "5");
        assert_eq!(headers.get("grpc-message").unwrap(), "no%20such%20thing");
        assert_eq!(headers.get("x-custom-trailer").unwrap(), "val");
        assert_eq!(headers.get("x-custom-bin").unwrap(), "3q2+7w");
        assert_eq!(headers.get("grpc-status-details-bin").unwrap(), "AA");
        for reserved in [
            "grpc-timeout",
            "grpc-encoding",
            "grpc-accept-encoding",
            "grpc-message-type",
        ] {
            assert!(
                headers.get(reserved).is_none(),
                "{reserved} must be stripped"
            );
        }
    }

    #[test]
    fn trailers_only_response_maps_all_status_code_errors() {
        let cases = [
            (StatusCodeError::Cancelled, "1"),
            (StatusCodeError::Unknown, "2"),
            (StatusCodeError::InvalidArgument, "3"),
            (StatusCodeError::DeadlineExceeded, "4"),
            (StatusCodeError::NotFound, "5"),
            (StatusCodeError::AlreadyExists, "6"),
            (StatusCodeError::PermissionDenied, "7"),
            (StatusCodeError::ResourceExhausted, "8"),
            (StatusCodeError::FailedPrecondition, "9"),
            (StatusCodeError::Aborted, "10"),
            (StatusCodeError::OutOfRange, "11"),
            (StatusCodeError::Unimplemented, "12"),
            (StatusCodeError::Internal, "13"),
            (StatusCodeError::Unavailable, "14"),
            (StatusCodeError::DataLoss, "15"),
            (StatusCodeError::Unauthenticated, "16"),
        ];
        for (code, expected) in cases {
            let response =
                trailers_only_response::<()>(Trailers::new(Err(StatusError::new(code, "msg"))));
            assert_eq!(
                response.headers().get("grpc-status").unwrap(),
                expected,
                "{code:?}"
            );
        }
    }
}
