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

use crate::client::transport::registry::GLOBAL_TRANSPORT_REGISTRY;
use crate::client::transport::ConnectedTransport;
use crate::client::transport::Transport;
use crate::client::transport::TransportOptions;
use crate::codec::BytesCodec;
use crate::rt::hyper_wrapper::{HyperCompatExec, HyperCompatTimer, HyperStream};
use crate::rt::BoxedTaskHandle;
use crate::rt::GrpcRuntime;
use crate::rt::TcpOptions;
use crate::service::Message;
use crate::service::Request as GrpcRequest;
use crate::service::Response as GrpcResponse;
use crate::{client::name_resolution::TCP_IP_NETWORK_TYPE, service::Service};
use bytes::Bytes;
use http::uri::PathAndQuery;
use http::Request as HttpRequest;
use http::Response as HttpResponse;
use http::Uri;
use hyper::client::conn::http2::Builder;
use hyper::client::conn::http2::SendRequest;
use std::any::Any;
use std::task::{Context, Poll};
use std::time::Instant;
use std::{error::Error, future::Future, net::SocketAddr, pin::Pin, str::FromStr};
use tokio::sync::oneshot;
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tonic::client::GrpcService;
use tonic::Request as TonicRequest;
use tonic::Response as TonicResponse;
use tonic::Streaming;
use tonic::{async_trait, body::Body, client::Grpc, Status};
use tower::buffer::{future::ResponseFuture as BufferResponseFuture, Buffer};
use tower::limit::{ConcurrencyLimitLayer, RateLimitLayer};
use tower::{util::BoxService, ServiceBuilder};
use tower_service::Service as TowerService;

#[cfg(test)]
mod test;

const DEFAULT_BUFFER_SIZE: usize = 1024;
pub(crate) type BoxError = Box<dyn Error + Send + Sync>;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

pub(crate) fn reg() {
    GLOBAL_TRANSPORT_REGISTRY.add_transport(TCP_IP_NETWORK_TYPE, TransportBuilder {});
}

struct TransportBuilder {}

struct TonicTransport {
    grpc: Grpc<TonicService>,
    task_handle: BoxedTaskHandle,
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
            let err = Status::internal("Failed to parse path");
            return create_error_response(err);
        };
        let mut grpc = self.grpc.clone();
        if let Err(e) = grpc.ready().await {
            // TODO: Figure out the exact situations under which the service
            // may return an error and re-evaluate the status code returned
            // below.
            let err = Status::unknown(format!("Service was not ready: {e}"));
            return create_error_response(err);
        };
        let request = convert_request(request);
        let response = grpc.streaming(request, path, BytesCodec {}).await;
        convert_response(response)
    }
}

/// Helper function to create an error response stream.
fn create_error_response(status: Status) -> GrpcResponse {
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

fn convert_response(res: Result<TonicResponse<Streaming<Bytes>>, Status>) -> GrpcResponse {
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

        let service = TonicTransport { grpc, task_handle };
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
