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
use std::future::pending;
use std::future::poll_fn;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Buf;
use bytes::Bytes;
use h2::Error as H2Error;
use http::Method;
use http::Request;
use http::StatusCode;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use hyper::Error as HyperError;
use hyper::client::conn::http2::Builder as ClientHttp2Builder;
use hyper::client::conn::http2::SendRequest;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::duplex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::timeout;

use super::Http2Config;
use super::HyperListener;
use crate::core::ConnectionInfo;
use crate::core::RecvMessage;
use crate::core::SendMessage;
use crate::credentials::ProtocolInfo;
use crate::credentials::SecurityInfo;
use crate::credentials::SecurityLevel;
use crate::credentials::ServerCredentials;
use crate::credentials::server::HandshakeOutput;
use crate::metadata::MetadataMap;
use crate::metadata::MetadataValue;
use crate::private::Internal;
use crate::rt::BoxEndpoint;
use crate::rt::EndpointIoStream;
use crate::rt::EndpointListener;
use crate::rt::GrpcRuntime;
use crate::rt::StreamEndpoint;
use crate::rt::address::ListenerAddress;
use crate::rt::default_runtime;
use crate::rt::hyper_wrapper::HyperCompatExec;
use crate::rt::hyper_wrapper::HyperStream;
use crate::server::BoxedRecvStream;
use crate::server::CallOptions;
use crate::server::DynHandle;
use crate::server::DynSendStream;
use crate::server::Handle;
use crate::server::Listener;
use crate::server::RecvStream;
use crate::server::RequestHeaders;
use crate::server::ResponseHeaders;
use crate::server::ResponseStreamItem;
use crate::server::SendOptions;
use crate::server::SendStream;
use crate::server::Server;
use crate::server::Trailers;
use crate::server::Transport;
use crate::status::StatusCodeError;
use crate::status::StatusError;

pub struct DummyHandle;

impl Handle for DummyHandle {
    async fn handle(
        &self,
        _headers: RequestHeaders,
        _options: CallOptions,
        _tx: &mut impl SendStream,
        _rx: impl RecvStream + 'static,
    ) -> Trailers {
        Trailers::new(Ok(()))
    }
}

pub struct BytesCapture {
    data: Option<Bytes>,
}

impl BytesCapture {
    pub fn new() -> Self {
        Self { data: None }
    }

    pub fn data(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }
}

impl RecvMessage for BytesCapture {
    fn decode(&mut self, data: &mut dyn Buf) -> Result<(), String> {
        self.data = Some(data.copy_to_bytes(data.remaining()));
        Ok(())
    }
}

pub struct TestSendMessage(Bytes);

impl TestSendMessage {
    pub fn new(data: Bytes) -> Self {
        Self(data)
    }
}

impl SendMessage for TestSendMessage {
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
        Ok(Box::new(self.0.clone()))
    }
}

/// Sends response headers, then never finishes.
pub struct PendingStreamHandle;

impl Handle for PendingStreamHandle {
    async fn handle(
        &self,
        _headers: RequestHeaders,
        _options: CallOptions,
        tx: &mut impl SendStream,
        _rx: impl RecvStream + 'static,
    ) -> Trailers {
        let _ = tx
            .send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default(),
            )
            .await;
        pending::<Trailers>().await
    }
}

/// A `POST` request with valid gRPC headers and an empty body.
pub fn valid_grpc_request() -> Request<Empty<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri("http://localhost/test.Service/Unary")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Empty::new())
        .unwrap()
}

/// Creates a connected `(server, client)` endpoint pair over an in-memory stream.
pub fn endpoint_pair() -> (BoxEndpoint, BoxEndpoint) {
    let (server, client) = duplex(64 * 1024);
    (
        Box::new(StreamEndpoint::new(
            server,
            "server".into(),
            "client".into(),
            "test",
        )),
        Box::new(StreamEndpoint::new(
            client,
            "client".into(),
            "server".into(),
            "test",
        )),
    )
}

/// Local address of an [`EndpointsListener`], which has no socket.
#[derive(Debug)]
struct InMemoryAddress;

impl ListenerAddress for InMemoryAddress {
    fn network(&self) -> &str {
        "inmemory"
    }
}

/// An [`EndpointListener`] that yields the endpoints sent on its channel. Once
/// the sender is dropped, `accept` waits forever, like a real listener with no
/// new clients.
struct EndpointsListener(Mutex<mpsc::UnboundedReceiver<BoxEndpoint>>);

#[crate::async_trait]
impl EndpointListener for EndpointsListener {
    async fn accept(&self) -> Result<BoxEndpoint, String> {
        match poll_fn(|cx| self.0.lock().unwrap().poll_recv(cx)).await {
            Some(endpoint) => Ok(endpoint),
            None => pending().await,
        }
    }

    fn local_addr(&self) -> Box<dyn ListenerAddress> {
        Box::new(InMemoryAddress)
    }
}

/// Creates a [`HyperListener`] that accepts the server endpoints sent on `endpoints`.
fn listener_with(
    endpoints: mpsc::UnboundedReceiver<BoxEndpoint>,
    creds: Arc<dyn ServerCredentials>,
) -> HyperListener {
    HyperListener::new(Box::new(EndpointsListener(Mutex::new(endpoints))), creds)
}

/// Server credentials that accept any endpoint with no security, for in-memory tests.
pub struct InsecureTestCreds;

#[crate::async_trait]
impl ServerCredentials for InsecureTestCreds {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo::new("insecure");
        &INFO
    }

    async fn accept(
        &self,
        source: BoxEndpoint,
        _runtime: GrpcRuntime,
        _token: Internal,
    ) -> Result<HandshakeOutput, String> {
        Ok(HandshakeOutput {
            endpoint: source,
            security: SecurityInfo::new("insecure").with_security_level(SecurityLevel::NoSecurity),
        })
    }
}

/// How long a test waits for a task that should finish. Tests that use it run
/// on the paused clock, so the wait is virtual and only fires on a hang.
pub const FINISH_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs a hyper HTTP/2 client handshake over `endpoint` and spawns the client
/// connection. The test sends on the returned `SendRequest`, and awaits the
/// `JoinHandle` after dropping it.
pub async fn connect_client(
    endpoint: BoxEndpoint,
    rt: &GrpcRuntime,
) -> Result<
    (
        SendRequest<Empty<Bytes>>,
        JoinHandle<Result<(), HyperError>>,
    ),
    HyperError,
> {
    let (sender, conn) = ClientHttp2Builder::new(HyperCompatExec { inner: rt.clone() })
        .handshake::<_, Empty<Bytes>>(HyperStream::new(endpoint))
        .await?;
    Ok((sender, tokio::spawn(conn)))
}

/// Sends on its channel when dropped, so a test can see a handler task being
/// aborted.
struct NotifyOnDrop(mpsc::UnboundedSender<()>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        // Can't report from `drop`. A send only fails once the test has
        // dropped its receiver, i.e. it no longer waits for this event.
        let _ = self.0.send(());
    }
}

/// Optionally sends response headers, then never finishes. Sends on `entered`
/// once per call that gets this far, and on `dropped` when that call's task is
/// aborted.
struct HangingHandler {
    send_headers: bool,
    entered: mpsc::UnboundedSender<()>,
    dropped: mpsc::UnboundedSender<()>,
}

#[crate::async_trait]
impl DynHandle for HangingHandler {
    async fn dyn_handle(
        &self,
        _headers: RequestHeaders,
        _options: CallOptions,
        tx: &mut dyn DynSendStream,
        _rx: BoxedRecvStream,
    ) -> Trailers {
        let _drop_guard = NotifyOnDrop(self.dropped.clone());
        if self.send_headers {
            tx.dyn_send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default(),
            )
            .await
            .unwrap();
        }
        self.entered.send(()).unwrap();
        pending::<()>().await;
        Trailers::new(Ok(()))
    }
}

struct StreamMessagesWithoutHeadersHandle {
    count: usize,
}

impl Handle for StreamMessagesWithoutHeadersHandle {
    async fn handle(
        &self,
        _headers: RequestHeaders,
        _options: CallOptions,
        tx: &mut impl SendStream,
        _rx: impl RecvStream + 'static,
    ) -> Trailers {
        for i in 0..self.count {
            let msg = TestSendMessage::new(Bytes::from(format!("msg-{i}")));
            tx.send(ResponseStreamItem::Message(&msg), SendOptions::default())
                .await
                .expect("sending message should succeed");
        }
        Trailers::new(Ok(()))
    }
}

#[tokio::test(start_paused = true)]
async fn keep_alive_closes_connection_when_client_stops_responding() {
    let rt = default_runtime();
    let config =
        Http2Config::new().keep_alive(Duration::from_millis(20), Duration::from_millis(50));
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds)).with_config(config);
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(Arc::new(DummyHandle), rt, Internal));

    // Send the HTTP/2 client preface and an empty SETTINGS frame, then never
    // read again, so the server's pings are never acked.
    let mut client = EndpointIoStream::new(client_ep);
    client
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00")
        .await
        .unwrap();

    // The first ping goes out at 20ms and its 50ms ack timeout ends at 70ms.
    sleep(Duration::from_millis(60)).await;
    assert!(!server_task.is_finished());
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should close the unresponsive connection")
        .unwrap();
    assert_eq!(outcome, Err("http2 error".to_string()));
}

#[tokio::test(start_paused = true)]
async fn keep_alive_keeps_responsive_connection_open() {
    let rt = default_runtime();
    let config =
        Http2Config::new().keep_alive(Duration::from_millis(20), Duration::from_millis(50));
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds)).with_config(config);
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(Arc::new(DummyHandle), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();
    send_req.ready().await.unwrap();

    // Dozens of ping rounds; the client acks every one.
    sleep(Duration::from_secs(1)).await;
    assert!(!server_task.is_finished());

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn send_message_without_headers_auto_flushes_headers_and_delivers_messages() {
    let rt = default_runtime();
    let creds = Arc::new(InsecureTestCreds);
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds);

    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let handler = Arc::new(StreamMessagesWithoutHeadersHandle { count: 3 });
    let server_task = tokio::spawn(conn.serve(handler, rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/Method")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();

    // 3 messages are more than the 1-slot response channel holds, so this
    // only completes if the default headers were flushed automatically.
    let collected = timeout(FINISH_TIMEOUT, async {
        let resp = send_req.send_request(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        resp.into_body().collect().await.unwrap()
    })
    .await
    .expect("sending messages without headers deadlocked");
    let trailers = collected.trailers().expect("response should have trailers");
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("0")
    );
    // Each message: compressed flag 0, 4-byte big-endian length 5, "msg-{i}".
    assert_eq!(
        collected.to_bytes(),
        Bytes::from_static(b"\0\0\0\0\x05msg-0\0\0\0\0\x05msg-1\0\0\0\0\x05msg-2"),
    );

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn oversize_request_message_surfaces_recv_error_to_handler() {
    let rt = default_runtime();
    let creds = Arc::new(InsecureTestCreds);
    let config = Http2Config::new().max_recv_message_size(16);
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds).with_config(config);

    // Handler that reads a message from rx and returns its own status when
    // `rx.dyn_next` yields `Some(Err(()))`.
    struct ReadOnceHandler;
    #[crate::async_trait]
    impl DynHandle for ReadOnceHandler {
        async fn dyn_handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut dyn DynSendStream,
            mut rx: BoxedRecvStream,
        ) -> Trailers {
            let mut msg = BytesCapture::new();
            match rx.dyn_next(&mut msg).await {
                Some(Ok(())) => Trailers::new(Ok(())),
                Some(Err(())) => Trailers::new(Err(StatusError::new(
                    StatusCodeError::Internal,
                    "generic handler stream failure",
                ))),
                None => Trailers::new(Err(StatusError::new(
                    StatusCodeError::Unknown,
                    "unexpected empty stream",
                ))),
            }
        }
    }

    let (server_ep, endpoint) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(Arc::new(ReadOnceHandler), rt.clone(), Internal));
    let (mut sender, client_conn) = ClientHttp2Builder::new(HyperCompatExec { inner: rt.clone() })
        .handshake::<_, Full<Bytes>>(HyperStream::new(endpoint))
        .await
        .unwrap();
    let client_task = tokio::spawn(client_conn);

    // Build a 32-byte LPM-framed request message (exceeding the 16-byte max_recv_message_size)
    let payload = b"0123456789abcdef0123456789abcdef"; // 32 bytes
    let mut lpm_frame = Vec::with_capacity(5 + payload.len());
    lpm_frame.push(0); // uncompressed flag
    lpm_frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    lpm_frame.extend_from_slice(payload);

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/Oversize")
        .header("content-type", "application/grpc")
        .body(Full::new(Bytes::from(lpm_frame)))
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("grpc-status").unwrap(), "13");
    assert_eq!(
        resp.headers().get("grpc-message").unwrap(),
        "generic%20handler%20stream%20failure"
    );
    let collected = resp.into_body().collect().await.unwrap();
    assert!(collected.trailers().is_none());

    drop(sender);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

/// Verifies that a client cancelling an RPC before `ResponseHeaders` (hyper
/// sends `RST_STREAM(CANCEL)` when the response future is dropped) aborts the
/// spawned handler task, and that the connection then closes cleanly.
#[tokio::test(start_paused = true)]
async fn client_rst_stream_before_headers_cancels_handler() {
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let (dropped_tx, mut dropped_rx) = mpsc::unbounded_channel();
    let handler = Arc::new(HangingHandler {
        send_headers: false,
        entered: entered_tx,
        dropped: dropped_tx,
    });
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(handler, rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/PreHeaders")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp_fut = send_req.send_request(req);
    entered_rx.recv().await.unwrap();
    // Cancels the RPC: hyper resets the stream with `RST_STREAM(CANCEL)`.
    drop(resp_fut);

    timeout(FINISH_TIMEOUT, dropped_rx.recv())
        .await
        .expect("handler task must be aborted on client RST_STREAM")
        .unwrap();

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

/// Verifies that a client cancelling an RPC after `ResponseHeaders` (h2 sends
/// `RST_STREAM(CANCEL)` when the `Response` is dropped) aborts the spawned
/// handler task, and that the connection then closes cleanly.
#[tokio::test(start_paused = true)]
async fn client_rst_stream_after_headers_cancels_handler() {
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let (dropped_tx, mut dropped_rx) = mpsc::unbounded_channel();
    let handler = Arc::new(HangingHandler {
        send_headers: true,
        entered: entered_tx,
        dropped: dropped_tx,
    });
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(handler, rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/PostHeaders")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    entered_rx.recv().await.unwrap();
    // Cancels the RPC: the body held the stream's last reference, so h2
    // resets the stream with `RST_STREAM(CANCEL)`.
    drop(resp);

    timeout(FINISH_TIMEOUT, dropped_rx.recv())
        .await
        .expect("handler task must be aborted on client RST_STREAM")
        .unwrap();

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

/// Verifies that dropping `HyperServingConnection` (e.g. when `max_connection_age_grace`
/// expires or the server shuts down) aborts all in-flight spawned `dyn_handle` tasks.
#[tokio::test(start_paused = true)]
async fn dropping_serving_connection_cancels_in_flight_handler() {
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let (dropped_tx, mut dropped_rx) = mpsc::unbounded_channel();
    let handler = Arc::new(HangingHandler {
        send_headers: true,
        entered: entered_tx,
        dropped: dropped_tx,
    });
    let rt = default_runtime();
    let creds = Arc::new(InsecureTestCreds);
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds);
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();

    let (drop_conn_tx, drop_conn_rx) = oneshot::channel::<()>();
    let serving = conn.serve(handler, rt.clone(), Internal);
    let server_task = tokio::spawn(async move {
        tokio::select! {
            outcome = serving => Some(outcome),
            // Dropping `serving` drops `HyperServingConnection`.
            _ = drop_conn_rx => None,
        }
    });
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/Stream")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    entered_rx.recv().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Drop `HyperServingConnection` on the server side while the RPC is in flight.
    drop_conn_tx.send(()).unwrap();

    timeout(FINISH_TIMEOUT, dropped_rx.recv())
        .await
        .expect("Dropping HyperServingConnection must abort in-flight handler tasks")
        .unwrap();
    let server_outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server task should end once the connection is dropped")
        .unwrap();
    assert_eq!(
        server_outcome, None,
        "the connection must be dropped, not finish"
    );

    // The client sees its in-flight response cut off: hyper reports a body
    // error whose cause is h2's I/O error.
    let mut body = resp.into_body();
    let err = body.frame().await.unwrap().unwrap_err();
    let cause = err.source().and_then(|e| e.downcast_ref::<H2Error>());
    assert!(
        cause.is_some_and(H2Error::is_io),
        "unexpected error: {err:?}"
    );

    drop(body);
    drop(send_req);
    let client_err = timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap_err();
    let io_err = client_err
        .source()
        .and_then(|e| e.downcast_ref::<io::Error>());
    assert_eq!(
        io_err.map(io::Error::kind),
        Some(io::ErrorKind::BrokenPipe),
        "unexpected error: {client_err:?}"
    );
}

/// Attaches custom ASCII and `-bin` metadata to its trailers. The method name
/// picks the response shape: streaming, trailers-only `Ok(())`, or
/// trailers-only error.
struct CustomTrailersHandler;

#[crate::async_trait]
impl DynHandle for CustomTrailersHandler {
    async fn dyn_handle(
        &self,
        headers: RequestHeaders,
        _options: CallOptions,
        tx: &mut dyn DynSendStream,
        _rx: BoxedRecvStream,
    ) -> Trailers {
        let mut trailer_meta = MetadataMap::new();
        trailer_meta.insert("x-custom-trailer", "trailer-val".parse().unwrap());
        trailer_meta.insert_bin(
            "x-custom-bin",
            MetadataValue::from_bytes(&[0xde, 0xad, 0xbe, 0xef]),
        );

        match headers.method_name() {
            "/test.Service/StreamingOk" => {
                tx.dyn_send(
                    ResponseStreamItem::Headers(ResponseHeaders::default()),
                    SendOptions::default(),
                )
                .await
                .unwrap();
                let msg = TestSendMessage::new(Bytes::from_static(b"hello"));
                tx.dyn_send(ResponseStreamItem::Message(&msg), SendOptions::default())
                    .await
                    .unwrap();
                Trailers::new(Ok(())).with_metadata(trailer_meta)
            }
            "/test.Service/TrailersOnlyError" => Trailers::new(Err(StatusError::new(
                StatusCodeError::Internal,
                "handler failed",
            )))
            .with_metadata(trailer_meta),
            _ => Trailers::new(Ok(())).with_metadata(trailer_meta),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn custom_trailer_metadata_preserved_on_streaming_response() {
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task =
        tokio::spawn(conn.serve(Arc::new(CustomTrailersHandler), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/StreamingOk")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let collected = resp.into_body().collect().await.unwrap();
    let trailers = collected.trailers().expect("expected HTTP/2 trailers");
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("0")
    );
    assert_eq!(
        trailers
            .get("x-custom-trailer")
            .and_then(|v| v.to_str().ok()),
        Some("trailer-val")
    );
    assert_eq!(
        trailers.get("x-custom-bin").and_then(|v| v.to_str().ok()),
        Some("3q2+7w")
    );

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn custom_trailer_metadata_preserved_on_trailers_only_response() {
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task =
        tokio::spawn(conn.serve(Arc::new(CustomTrailersHandler), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/TrailersOnlyOk")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let headers = resp.headers();
    assert_eq!(
        headers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("0")
    );
    assert_eq!(
        headers
            .get("x-custom-trailer")
            .and_then(|v| v.to_str().ok()),
        Some("trailer-val")
    );
    assert_eq!(
        headers.get("x-custom-bin").and_then(|v| v.to_str().ok()),
        Some("3q2+7w")
    );
    let collected = resp.into_body().collect().await.unwrap();
    assert!(collected.to_bytes().is_empty());

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn custom_trailer_metadata_preserved_on_error_response() {
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task =
        tokio::spawn(conn.serve(Arc::new(CustomTrailersHandler), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/TrailersOnlyError")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let headers = resp.headers();
    assert_eq!(
        headers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("13")
    );
    assert_eq!(
        headers.get("grpc-message").and_then(|v| v.to_str().ok()),
        Some("handler%20failed")
    );
    assert_eq!(
        headers
            .get("x-custom-trailer")
            .and_then(|v| v.to_str().ok()),
        Some("trailer-val")
    );
    assert_eq!(
        headers.get("x-custom-bin").and_then(|v| v.to_str().ok()),
        Some("3q2+7w")
    );
    let collected = resp.into_body().collect().await.unwrap();
    assert!(collected.to_bytes().is_empty());

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

/// Verifies that handler-supplied `ResponseHeaders` cannot inject gRPC reserved
/// transport headers (such as `grpc-status` or `grpc-message`).
///
/// Injecting `grpc-status` in initial headers causes the client to treat the response
/// as trailers-only, dropping all response messages. Stripping it preserves message delivery.
#[tokio::test(start_paused = true)]
async fn outgoing_reserved_headers_stripped_and_messages_delivered() {
    struct ReservedHeadersHandler;

    #[crate::async_trait]
    impl DynHandle for ReservedHeadersHandler {
        async fn dyn_handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            tx: &mut dyn DynSendStream,
            _rx: BoxedRecvStream,
        ) -> Trailers {
            let mut meta = MetadataMap::new();
            meta.insert("grpc-status", "0".parse().unwrap());
            meta.insert("grpc-message", "handler-msg".parse().unwrap());
            meta.insert("x-custom-header", "custom-value".parse().unwrap());

            let mut resp_headers = ResponseHeaders::default();
            *resp_headers.metadata_mut() = meta;

            tx.dyn_send(
                ResponseStreamItem::Headers(resp_headers),
                SendOptions::default(),
            )
            .await
            .unwrap();

            for i in 0..3 {
                let msg = TestSendMessage::new(Bytes::from(format!("msg-{i}")));
                tx.dyn_send(ResponseStreamItem::Message(&msg), SendOptions::default())
                    .await
                    .unwrap();
            }

            Trailers::new(Ok(()))
        }
    }

    let rt = default_runtime();
    let creds = Arc::new(InsecureTestCreds);
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds);

    let (server_ep, endpoint) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task =
        tokio::spawn(conn.serve(Arc::new(ReservedHeadersHandler), rt.clone(), Internal));
    let (mut sender, client_task) = connect_client(endpoint, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/ReservedHeaders")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    // 1. Verify initial HEADERS frame:
    // - x-custom-header is preserved.
    // - content-type is application/grpc.
    // - grpc-status and grpc-message are STRIPPED.
    let resp_headers = resp.headers();
    assert_eq!(
        resp_headers
            .get("x-custom-header")
            .and_then(|v| v.to_str().ok()),
        Some("custom-value")
    );
    assert_eq!(
        resp_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/grpc")
    );
    assert!(
        resp_headers.get("grpc-status").is_none(),
        "grpc-status must be stripped from initial ResponseHeaders"
    );
    assert!(
        resp_headers.get("grpc-message").is_none(),
        "grpc-message must be stripped from initial ResponseHeaders"
    );

    // 2. Verify all 3 messages are received (not dropped due to trailers-only confusion):
    let collected = resp.into_body().collect().await.unwrap();
    let trailers = collected
        .trailers()
        .cloned()
        .expect("expected HTTP/2 trailers");
    // 3 messages: "msg-0" (5 bytes + 5 LPM header = 10 bytes) * 3 = 30 bytes
    assert_eq!(collected.to_bytes().len(), 30);

    // 3. Verify trailers contain grpc-status: 0:
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("0")
    );

    drop(sender);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

enum HandshakeTestMode {
    FailFirstThenSucceed,
    StallFirstThenSucceed,
}

struct TestHandshakeCreds {
    attempt: AtomicUsize,
    mode: HandshakeTestMode,
    first_handshake_started: Notify,
}

impl TestHandshakeCreds {
    fn new(mode: HandshakeTestMode) -> Self {
        Self {
            attempt: AtomicUsize::new(0),
            mode,
            first_handshake_started: Notify::new(),
        }
    }
}

#[crate::async_trait]
impl ServerCredentials for TestHandshakeCreds {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo::new("test");
        &INFO
    }

    async fn accept(
        &self,
        source: BoxEndpoint,
        runtime: GrpcRuntime,
        token: Internal,
    ) -> Result<HandshakeOutput, String> {
        let idx = self.attempt.fetch_add(1, Ordering::SeqCst);
        if idx == 0 {
            self.first_handshake_started.notify_one();
        }
        match self.mode {
            HandshakeTestMode::FailFirstThenSucceed if idx == 0 => {
                Err("simulated TLS handshake failure".to_string())
            }
            HandshakeTestMode::StallFirstThenSucceed if idx == 0 => {
                pending::<Result<HandshakeOutput, String>>().await
            }
            _ => InsecureTestCreds.accept(source, runtime, token).await,
        }
    }
}

#[tokio::test(start_paused = true)]
async fn bad_handshake_does_not_kill_accept_loop() {
    let rt = default_runtime();
    let creds = Arc::new(TestHandshakeCreds::new(
        HandshakeTestMode::FailFirstThenSucceed,
    ));
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = Server::new(DummyHandle, rt.clone());
    let server_task = tokio::spawn(async move {
        server
            .serve_with_shutdown(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    // 1. First client connection triggers a credential handshake failure, and
    // the server closes it.
    let (bad_server_ep, bad_client) = endpoint_pair();
    accept_tx.send(bad_server_ep).unwrap();
    creds.first_handshake_started.notified().await;
    let mut bad_client = EndpointIoStream::new(bad_client);
    let mut buf = [0u8; 1];
    let read = timeout(FINISH_TIMEOUT, bad_client.read(&mut buf))
        .await
        .expect("server should close the failed connection")
        .unwrap();
    assert_eq!(read, 0, "expected EOF after the failed handshake");

    // 2. Second client connection must still be accepted and complete HTTP/2 handshake.
    let (good_server_ep, good_client) = endpoint_pair();
    accept_tx.send(good_server_ep).unwrap();
    let (mut send_req, client_task) = connect_client(good_client, &rt)
        .await
        .expect("server should still accept connections after a failed handshake");
    send_req.ready().await.unwrap();

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    shutdown_tx.send(()).unwrap();
    timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should stop on shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn stalled_handshake_does_not_block_subsequent_connections() {
    let rt = default_runtime();
    let creds = Arc::new(TestHandshakeCreds::new(
        HandshakeTestMode::StallFirstThenSucceed,
    ));
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = Server::new(DummyHandle, rt.clone());
    let server_task = tokio::spawn(async move {
        server
            .serve_with_shutdown(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    // 1. Stalled client connects and stalls indefinitely in credential handshake.
    let (stalled_server_ep, stalled_client) = endpoint_pair();
    accept_tx.send(stalled_server_ep).unwrap();
    creds.first_handshake_started.notified().await;

    // 2. Subsequent client connects and must not be blocked by the stalled handshake.
    let (good_server_ep, good_client) = endpoint_pair();
    accept_tx.send(good_server_ep).unwrap();
    let (send_req, client_task) = timeout(Duration::from_millis(500), async {
        let (mut send_req, client_task) = connect_client(good_client, &rt).await.unwrap();
        send_req.ready().await.unwrap();
        (send_req, client_task)
    })
    .await
    .expect("subsequent client was blocked by stalled handshake");
    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();

    // 3. Graceful shutdown must cleanly abort the stalled handshake task, which
    // closes the stalled connection.
    shutdown_tx.send(()).unwrap();
    timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("graceful shutdown hung on stalled handshake")
        .unwrap()
        .unwrap();
    let mut stalled_client = EndpointIoStream::new(stalled_client);
    let mut buf = [0u8; 1];
    let read = timeout(FINISH_TIMEOUT, stalled_client.read(&mut buf))
        .await
        .expect("stalled connection should be closed on shutdown")
        .unwrap();
    assert_eq!(read, 0, "expected EOF on the stalled connection");
}

#[tokio::test(start_paused = true)]
async fn handshake_timeout_terminates_stalled_connection() {
    let rt = default_runtime();
    let creds = Arc::new(TestHandshakeCreds::new(
        HandshakeTestMode::StallFirstThenSucceed,
    ));
    let config = Http2Config::new().handshake_timeout(Duration::from_millis(50));
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds).with_config(config);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = Server::new(DummyHandle, rt.clone());
    let server_task = tokio::spawn(async move {
        server
            .serve_with_shutdown(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    // 1. Client connects and stalls in handshake.
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let mut client = EndpointIoStream::new(client_ep);

    // 2. While the read is pending, the paused clock auto-advances to the 50ms
    // handshake_timeout, and the server drops the connection task and its endpoint.
    // Reading from client should see EOF (0 bytes read).
    let mut buf = [0u8; 1];
    let read = timeout(Duration::from_millis(200), client.read(&mut buf))
        .await
        .expect("client read timed out waiting for server socket close")
        .unwrap();
    assert_eq!(read, 0, "expected EOF after handshake timeout");

    shutdown_tx.send(()).unwrap();
    timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should stop on shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn malformed_bin_header_returns_internal_error_without_invoking_handler() {
    let rt = default_runtime();
    let creds = Arc::new(InsecureTestCreds);
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, creds);

    let handler_called = Arc::new(AtomicBool::new(false));
    let handler_called_clone = handler_called.clone();

    struct TrackingHandler(Arc<AtomicBool>);
    #[crate::async_trait]
    impl DynHandle for TrackingHandler {
        async fn dyn_handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut dyn DynSendStream,
            _rx: BoxedRecvStream,
        ) -> Trailers {
            self.0.store(true, Ordering::SeqCst);
            Trailers::new(Ok(()))
        }
    }

    let (server_ep, endpoint) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let handler = Arc::new(TrackingHandler(handler_called_clone));
    let server_task = tokio::spawn(conn.serve(handler, rt.clone(), Internal));
    let (mut sender, client_task) = connect_client(endpoint, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/CorruptBin")
        .header("content-type", "application/grpc")
        .header("x-corrupt-bin", "!!!not-valid-base64!!!")
        .body(Empty::<Bytes>::new())
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("grpc-status").unwrap(), "13");
    let message = resp
        .headers()
        .get("grpc-message")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        message.starts_with("error%20decoding%20request%20metadata"),
        "unexpected grpc-message: {message}"
    );
    let collected = resp.into_body().collect().await.unwrap();
    assert!(collected.trailers().is_none());
    assert!(
        !handler_called.load(Ordering::SeqCst),
        "Handler must not be invoked when request metadata decoding fails"
    );

    drop(sender);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn request_reserved_headers_extracted_and_filtered_from_metadata() {
    /// Sends the `RequestHeaders` of the first request it handles over `tx`.
    struct CaptureRequestHeadersHandle {
        tx: Mutex<Option<oneshot::Sender<RequestHeaders>>>,
    }

    impl Handle for CaptureRequestHeadersHandle {
        async fn handle(
            &self,
            headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut impl SendStream,
            _rx: impl RecvStream + 'static,
        ) -> Trailers {
            if let Some(tx) = self.tx.lock().unwrap().take() {
                tx.send(headers).unwrap();
            }
            Trailers::new(Ok(()))
        }
    }

    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let (headers_tx, headers_rx) = oneshot::channel();
    let handler = Arc::new(CaptureRequestHeadersHandle {
        tx: Mutex::new(Some(headers_tx)),
    });
    let server_task = tokio::spawn(conn.serve(handler, rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/InspectHeaders")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("grpc-timeout", "5S")
        .header("grpc-encoding", "identity")
        .header("grpc-accept-encoding", "gzip,identity")
        .header("user-agent", "custom-agent/1.0")
        .header("x-custom-ascii", "allowed-value")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    let headers = headers_rx.await.unwrap();

    assert_eq!(headers.timeout(), Some(Duration::from_secs(5)));
    assert_eq!(headers.encoding(), Some("identity"));
    assert_eq!(headers.accept_encoding(), Some("gzip,identity"));
    let md = headers.metadata();
    assert!(md.get("content-type").is_none());
    assert!(md.get("te").is_none());
    assert!(md.get("grpc-timeout").is_none());
    assert!(md.get("grpc-encoding").is_none());
    assert!(md.get("grpc-accept-encoding").is_none());
    assert_eq!(
        md.get("user-agent").map(|v| v.to_str()),
        Some("custom-agent/1.0")
    );
    assert_eq!(
        md.get("x-custom-ascii").map(|v| v.to_str()),
        Some("allowed-value")
    );

    drop(resp);
    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn response_content_type_overwritten_when_handler_sets_it() {
    /// Sets its own `content-type` and an `x-echo` header on the response.
    struct ContentTypeOverrideHandle;

    #[crate::async_trait]
    impl DynHandle for ContentTypeOverrideHandle {
        async fn dyn_handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            tx: &mut dyn DynSendStream,
            _rx: BoxedRecvStream,
        ) -> Trailers {
            let mut resp_md = MetadataMap::new();
            resp_md.insert("content-type", "application/grpc+override".parse().unwrap());
            resp_md.insert("x-echo", "ok".parse().unwrap());
            tx.dyn_send(
                ResponseStreamItem::Headers(ResponseHeaders::default().with_metadata(resp_md)),
                SendOptions::default(),
            )
            .await
            .unwrap();
            let msg = TestSendMessage::new(Bytes::from_static(b"reply"));
            tx.dyn_send(ResponseStreamItem::Message(&msg), SendOptions::default())
                .await
                .unwrap();
            Trailers::new(Ok(()))
        }
    }

    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task =
        tokio::spawn(conn.serve(Arc::new(ContentTypeOverrideHandle), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/OverrideContentType")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let content_types: Vec<_> = resp
        .headers()
        .get_all("content-type")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(
        content_types,
        vec!["application/grpc"],
        "Expected single overwritten content-type header on response"
    );
    assert_eq!(
        resp.headers().get("x-echo").and_then(|v| v.to_str().ok()),
        Some("ok")
    );
    let collected = resp.into_body().collect().await.unwrap();
    let trailers = collected.trailers().expect("expected HTTP/2 trailers");
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("0")
    );

    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn invalid_content_type_rejected_with_unsupported_media_type() {
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(Arc::new(DummyHandle), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/BadCt")
        .header("content-type", "application/grpc-web")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    drop(resp);
    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn invalid_method_rejected_with_method_not_allowed() {
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(Arc::new(DummyHandle), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri("http://localhost/test.Service/BadMethod")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);

    drop(resp);
    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn unsupported_grpc_encoding_rejected_with_unimplemented() {
    let rt = default_runtime();
    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let listener = listener_with(accept_rx, Arc::new(InsecureTestCreds));
    let (server_ep, client_ep) = endpoint_pair();
    accept_tx.send(server_ep).unwrap();
    let conn = listener.accept(Internal).await.unwrap().unwrap();
    let server_task = tokio::spawn(conn.serve(Arc::new(DummyHandle), rt.clone(), Internal));
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/Compressed")
        .header("content-type", "application/grpc")
        .header("grpc-encoding", "gzip")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = send_req.send_request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("grpc-status").unwrap(), "12");
    assert_eq!(
        resp.headers().get("grpc-message").unwrap(),
        "unsupported%20grpc-encoding:%20gzip"
    );

    drop(resp);
    drop(send_req);
    timeout(FINISH_TIMEOUT, client_task)
        .await
        .expect("client connection should close")
        .unwrap()
        .unwrap();
    let outcome = timeout(FINISH_TIMEOUT, server_task)
        .await
        .expect("server should finish once the client closes")
        .unwrap();
    assert_eq!(outcome, Ok(()));
}

/// Sends the `ConnectionInfo` of the first request it handles over `tx`.
struct CaptureConnectionInfoHandle {
    tx: Mutex<Option<oneshot::Sender<ConnectionInfo>>>,
}

impl Handle for CaptureConnectionInfoHandle {
    async fn handle(
        &self,
        headers: RequestHeaders,
        _options: CallOptions,
        _tx: &mut impl SendStream,
        _rx: impl RecvStream + 'static,
    ) -> Trailers {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(headers.connection_info().clone());
        }
        Trailers::new(Ok(()))
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unix_listener_reports_unix_connection_info_to_handler() {
    let rt = default_runtime();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hyper.sock");
    let listener = HyperListener::new_unix_listener(
        path.clone(),
        Arc::new(crate::credentials::LocalServerCredentials::new()),
        crate::rt::UnixSocketOptions::default(),
        &rt,
    )
    .await
    .unwrap();
    let (info_tx, info_rx) = oneshot::channel();
    let handler = Arc::new(CaptureConnectionInfoHandle {
        tx: Mutex::new(Some(info_tx)),
    });

    let accept_rt = rt.clone();
    let server_task = tokio::spawn(async move {
        let conn = listener.accept(Internal).await.unwrap().unwrap();
        conn.serve(handler, accept_rt, Internal).await
    });

    let client_ep = rt
        .unix_stream(path.clone(), crate::rt::UnixSocketOptions::default())
        .await
        .unwrap();
    let (mut send_req, client_task) = connect_client(client_ep, &rt).await.unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("http://localhost/test.Service/Method")
        .header("content-type", "application/grpc")
        .body(Empty::<Bytes>::new())
        .unwrap();

    let resp = send_req.send_request(req).await.unwrap();
    let info = info_rx.await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(info.local_address().network_type, "unix");
    assert_eq!(info.remote_address().network_type, "unix");
    assert_eq!(&*info.local_address().address, path.to_str().unwrap());
    assert_eq!(&*info.remote_address().address, "");

    drop(resp);
    drop(send_req);
    client_task.await.unwrap().unwrap();
    let server_result = server_task.await.unwrap();
    // The client closes first. On macOS, `shutdown()` on a Unix socket whose
    // peer has closed fails with ENOTCONN, which surfaces as a connection
    // error. tokio hides this for TCP but not for Unix sockets; see
    // https://github.com/tokio-rs/tokio/issues/4665.
    if cfg!(target_os = "macos") {
        assert!(
            matches!(
                server_result.as_ref().map_err(String::as_str),
                Ok(()) | Err("connection error")
            ),
            "{server_result:?}"
        );
    } else {
        assert_eq!(server_result, Ok(()));
    }
}
