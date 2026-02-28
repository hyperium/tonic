/*
 *
 * Copyright 2025 gRPC authors.
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

use std::any::Any;
use std::error::Error;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::task::Context;
use std::task::Poll;
use std::time::Instant;

use bytes::Buf;
use bytes::BufMut as _;
use bytes::Bytes;
use http::Request as HttpRequest;
use http::Response as HttpResponse;
use http::Uri;
use http::uri::PathAndQuery;
use hyper::client::conn::http2::Builder;
use hyper::client::conn::http2::SendRequest;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Request as TonicRequest;
use tonic::Response as TonicResponse;
use tonic::Status as TonicStatus;
use tonic::Streaming;
use tonic::async_trait;
use tonic::body::Body;
use tonic::client::Grpc;
use tonic::client::GrpcService;
use tonic::codec::Codec;
use tonic::codec::Decoder;
use tonic::codec::EncodeBuf;
use tonic::codec::Encoder;
use tower::ServiceBuilder;
use tower::buffer::Buffer;
use tower::buffer::future::ResponseFuture as BufferResponseFuture;
use tower::limit::ConcurrencyLimitLayer;
use tower::limit::RateLimitLayer;
use tower::util::BoxService;
use tower_service::Service as TowerService;

use crate::Status;
use crate::StatusCode;
use crate::client::CallOptions;
use crate::client::Invoke;
use crate::client::RecvStream;
use crate::client::SendOptions;
use crate::client::SendStream;
use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
use crate::client::transport::ConnectedTransport;
use crate::client::transport::Transport;
use crate::client::transport::TransportOptions;
use crate::client::transport::registry::GLOBAL_TRANSPORT_REGISTRY;
use crate::codec::BytesCodec;
use crate::core::ClientResponseStreamItem;
use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::ResponseHeaders;
use crate::core::SendMessage;
use crate::core::Trailers;
use crate::rt::BoxedTaskHandle;
use crate::rt::GrpcRuntime;
use crate::rt::TcpOptions;
use crate::rt::hyper_wrapper::HyperCompatExec;
use crate::rt::hyper_wrapper::HyperCompatTimer;
use crate::rt::hyper_wrapper::HyperStream;
use crate::service::Message;
use crate::service::Request as GrpcRequest;
use crate::service::Response as GrpcResponse;
use crate::service::Service;

#[cfg(test)]
mod test;

const DEFAULT_BUFFER_SIZE: usize = 1024;
pub(crate) type BoxError = Box<dyn Error + Send + Sync>;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, TonicStatus>> + Send>>;

pub(crate) fn reg() {
    GLOBAL_TRANSPORT_REGISTRY.add_transport(TCP_IP_NETWORK_TYPE, TransportBuilder {});
}

struct TransportBuilder {}

struct TonicTransport {
    grpc: Grpc<TonicService>,
    task_handle: BoxedTaskHandle,
    runtime: GrpcRuntime,
}

impl Drop for TonicTransport {
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

#[async_trait]
impl Service for TonicTransport {
    async fn call(&self, method: String, request: GrpcRequest) -> GrpcResponse {
        let Ok(path) = PathAndQuery::from_maybe_shared(method) else {
            let err = TonicStatus::internal("Failed to parse path");
            return create_error_response(err);
        };
        let mut grpc = self.grpc.clone();
        if let Err(e) = grpc.ready().await {
            // TODO: Figure out the exact situations under which the service
            // may return an error and re-evaluate the status code returned
            // below.
            let err = TonicStatus::unknown(format!("Service was not ready: {e}"));
            return create_error_response(err);
        };
        let request = convert_request(request);
        let response = grpc.streaming(request, path, BytesCodec {}).await;
        convert_response(response)
    }
}

impl Invoke for TonicTransport {
    type SendStream = TonicSendStream;
    type RecvStream = TonicRecvStream;

    async fn invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        let (req_tx, req_rx) = mpsc::channel(1);
        let request_stream = ReceiverStream::new(req_rx);
        let mut request = TonicRequest::new(Box::pin(request_stream));
        *request.metadata_mut() = headers.metadata().clone();

        let Ok(path) = PathAndQuery::from_maybe_shared(headers.method_name().clone()) else {
            return err_streams(Status::new(StatusCode::Internal, "invalid path"));
        };

        let mut grpc = self.grpc.clone();
        if let Err(e) = grpc.ready().await {
            return err_streams(Status::new(
                StatusCode::Unavailable,
                format!("Service was not ready: {e}"),
            ));
        }

        // Note that Tonic's streaming call blocks until the server's headers
        // are received.  We must return a working send (and, consequently,
        // recv) stream before this to allow the application to write its
        // request(s), so we need to spawn a task for this period of time, and
        // then we send the response (headers, stream) to the TonicRecvStream
        // when it is available.
        let (resp_tx, resp_rx) = oneshot::channel();
        self.runtime.spawn(Box::pin(async move {
            let response = grpc.streaming(request, path, BufCodec {}).await;
            let _ = resp_tx.send(response);
        }));

        (
            TonicSendStream { sender: Ok(req_tx) },
            TonicRecvStream {
                receiver: None,
                error: None,
                response_rx: Some(resp_rx),
            },
        )
    }
}

// Converts from a tonic status to a grpc-crate status.
fn from_tonic_status(status: TonicStatus) -> Status {
    Status::new(StatusCode::from(status.code() as i32), status.message())
}

struct TonicSendStream {
    sender: Result<mpsc::Sender<Box<dyn Buf + Send + Sync>>, ()>,
}

impl SendStream for TonicSendStream {
    async fn send(&mut self, msg: &dyn SendMessage, options: SendOptions) -> Result<(), ()> {
        if let Ok(tx) = &self.sender
            && let Ok(buf) = msg.encode()
            && tx.send(buf).await.is_ok()
        {
            if options.final_msg {
                self.sender = Err(());
            }
            return Ok(());
        }
        Err(())
    }
}

struct TonicRecvStream {
    error: Option<Status>,
    response_rx: Option<oneshot::Receiver<Result<tonic::Response<Streaming<Bytes>>, TonicStatus>>>,
    receiver: Option<Streaming<Bytes>>,
}

impl RecvStream for TonicRecvStream {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> ClientResponseStreamItem {
        if let Some(error) = self.error.take() {
            return ClientResponseStreamItem::Trailers(Trailers::new(error));
        }

        if let Some(rx) = self.response_rx.take() {
            match rx.await {
                Ok(Ok(response)) => {
                    let (metadata, stream, _extensions) = response.into_parts();
                    self.receiver = Some(stream);
                    return ClientResponseStreamItem::Headers(
                        ResponseHeaders::new().with_metadata(metadata),
                    );
                }
                Ok(Err(status)) => {
                    return ClientResponseStreamItem::Trailers(Trailers::new(from_tonic_status(
                        status,
                    )));
                }
                Err(_) => {
                    return ClientResponseStreamItem::Trailers(Trailers::new(Status::new(
                        StatusCode::Unknown,
                        "Task cancelled",
                    )));
                }
            }
        }

        let Some(mut stream) = self.receiver.take() else {
            return ClientResponseStreamItem::StreamClosed;
        };

        let Some(resp) = stream.next().await else {
            return ClientResponseStreamItem::Trailers(Trailers::new(Status::new(
                StatusCode::Ok,
                "",
            )));
        };

        match resp {
            Ok(mut buf) => match msg.decode(&mut buf) {
                Ok(()) => {
                    // More messages may remain in the stream; set receiver again.
                    self.receiver = Some(stream);
                    ClientResponseStreamItem::Message(())
                }
                // TODO: in this case, tonic believes the stream is still
                // running, but our decoding failed -- do we need to terminate
                // the request stream now even though the Streaming is dropped?
                Err(e) => ClientResponseStreamItem::Trailers(Trailers::new(Status::new(
                    StatusCode::Internal,
                    "error decoding response: {",
                ))),
            },
            Err(status) => {
                ClientResponseStreamItem::Trailers(Trailers::new(from_tonic_status(status)))
            }
        }
    }
}

fn err_streams(status: Status) -> (TonicSendStream, TonicRecvStream) {
    (
        TonicSendStream { sender: Err(()) },
        TonicRecvStream {
            receiver: None,
            response_rx: None,
            error: Some(status),
        },
    )
}

/// Helper function to create an error response stream.
fn create_error_response(status: TonicStatus) -> GrpcResponse {
    let stream = tokio_stream::once(Err(status));
    TonicResponse::new(Box::pin(stream))
}

fn convert_request(req: GrpcRequest) -> TonicRequest<Pin<Box<dyn Stream<Item = Bytes> + Send>>> {
    let (metadata, extensions, stream) = req.into_parts();

    let bytes_stream = Box::pin(stream.filter_map(|msg| {
        if let Ok(bytes) = (msg as Box<dyn Any>).downcast::<Bytes>() {
            Some(*bytes)
        } else {
            // If it fails, log the error and return None to filter it out.
            eprintln!("A message could not be downcast to Bytes and was skipped.");
            None
        }
    }));

    TonicRequest::from_parts(metadata, extensions, bytes_stream as _)
}

fn convert_response(res: Result<TonicResponse<Streaming<Bytes>>, TonicStatus>) -> GrpcResponse {
    let response = match res {
        Ok(s) => s,
        Err(e) => {
            let stream = tokio_stream::once(Err(e));
            return TonicResponse::new(Box::pin(stream));
        }
    };
    let (metadata, stream, extensions) = response.into_parts();
    let message_stream: BoxStream<Box<dyn Message>> = Box::pin(stream.map(|msg| {
        msg.map(|b| {
            let msg: Box<dyn Message> = Box::new(b);
            msg
        })
    }));
    TonicResponse::from_parts(metadata, message_stream, extensions)
}

#[async_trait]
impl Transport for TransportBuilder {
    async fn connect(
        &self,
        address: String,
        runtime: GrpcRuntime,
        opts: &TransportOptions,
    ) -> Result<ConnectedTransport, String> {
        let runtime = runtime.clone();
        let mut settings = Builder::<HyperCompatExec>::new(HyperCompatExec {
            inner: runtime.clone(),
        })
        .timer(HyperCompatTimer {
            inner: runtime.clone(),
        })
        .initial_stream_window_size(opts.init_stream_window_size)
        .initial_connection_window_size(opts.init_connection_window_size)
        .keep_alive_interval(opts.http2_keep_alive_interval)
        .clone();

        if let Some(val) = opts.http2_keep_alive_timeout {
            settings.keep_alive_timeout(val);
        }

        if let Some(val) = opts.http2_keep_alive_while_idle {
            settings.keep_alive_while_idle(val);
        }

        if let Some(val) = opts.http2_adaptive_window {
            settings.adaptive_window(val);
        }

        if let Some(val) = opts.http2_max_header_list_size {
            settings.max_header_list_size(val);
        }

        let addr: SocketAddr = SocketAddr::from_str(&address).map_err(|err| err.to_string())?;
        let tcp_stream_fut = runtime.tcp_stream(
            addr,
            TcpOptions {
                enable_nodelay: opts.tcp_nodelay,
                keepalive: opts.tcp_keepalive,
            },
        );
        let tcp_stream = if let Some(deadline) = opts.connect_deadline {
            let timeout = deadline.saturating_duration_since(Instant::now());
            tokio::select! {
            _ = runtime.sleep(timeout) => {
                return Err("timed out waiting for TCP stream to connect".to_string())
            }
            tcp_stream = tcp_stream_fut => { tcp_stream? }
            }
        } else {
            tcp_stream_fut.await?
        };
        let tcp_stream = HyperStream::new(tcp_stream);

        let (sender, connection) = settings
            .handshake(tcp_stream)
            .await
            .map_err(|err| err.to_string())?;
        let (tx, rx) = oneshot::channel();

        let task_handle = runtime.spawn(Box::pin(async move {
            if let Err(err) = connection.await {
                let _ = tx.send(Err(err.to_string()));
            } else {
                let _ = tx.send(Ok(()));
            }
        }));
        let sender = SendRequestWrapper::from(sender);

        let service = ServiceBuilder::new()
            .option_layer(opts.concurrency_limit.map(ConcurrencyLimitLayer::new))
            .option_layer(opts.rate_limit.map(|(l, d)| RateLimitLayer::new(l, d)))
            .map_err(Into::<BoxError>::into)
            .service(sender);

        let service = BoxService::new(service);
        let (service, worker) = Buffer::pair(service, DEFAULT_BUFFER_SIZE);
        runtime.spawn(Box::pin(worker));
        let uri =
            Uri::from_maybe_shared(format!("http://{}", &address)).map_err(|e| e.to_string())?; // TODO: err msg
        let grpc = Grpc::with_origin(TonicService { inner: service }, uri);

        let service = TonicTransport {
            grpc,
            task_handle,
            runtime,
        };
        Ok(ConnectedTransport {
            service: Box::new(service),
            disconnection_listener: rx,
        })
    }
}

struct SendRequestWrapper {
    inner: SendRequest<Body>,
}

impl From<SendRequest<Body>> for SendRequestWrapper {
    fn from(inner: SendRequest<Body>) -> Self {
        Self { inner }
    }
}

impl TowerService<HttpRequest<Body>> for SendRequestWrapper {
    type Response = HttpResponse<Body>;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let fut = self.inner.send_request(req);
        Box::pin(async move { fut.await.map_err(Into::into).map(|res| res.map(Body::new)) })
    }
}

#[derive(Clone)]
struct TonicService {
    inner: Buffer<http::Request<Body>, BoxFuture<'static, Result<http::Response<Body>, BoxError>>>,
}

impl GrpcService<Body> for TonicService {
    type ResponseBody = Body;
    type Error = BoxError;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        ResponseFuture {
            inner: tower::Service::call(&mut self.inner, request),
        }
    }
}

/// A future that resolves to an HTTP response.
///
/// This is returned by the `Service::call` on [`Channel`].
pub(crate) struct ResponseFuture {
    inner: BufferResponseFuture<BoxFuture<'static, Result<HttpResponse<Body>, BoxError>>>,
}

impl Future for ResponseFuture {
    type Output = Result<http::Response<Body>, BoxError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx)
    }
}

pub(crate) struct BufCodec {}

impl Codec for BufCodec {
    type Encode = Box<dyn Buf + Send + Sync>;
    type Decode = Bytes;
    type Encoder = BufEncoder;
    type Decoder = BytesDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        BufEncoder {}
    }

    fn decoder(&mut self) -> Self::Decoder {
        BytesDecoder {}
    }
}

pub struct BytesEncoder {}

impl Encoder for BytesEncoder {
    type Item = Bytes;
    type Error = TonicStatus;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        dst.put_slice(&item);
        Ok(())
    }
}

pub struct BufEncoder {}

impl Encoder for BufEncoder {
    type Item = Box<dyn Buf + Send + Sync>;
    type Error = TonicStatus;

    fn encode(&mut self, mut item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        dst.put(&mut *item);
        Ok(())
    }
}

#[derive(Debug)]
pub struct BytesDecoder {}

impl Decoder for BytesDecoder {
    type Item = Bytes;
    type Error = TonicStatus;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Self::Item>, Self::Error> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}
