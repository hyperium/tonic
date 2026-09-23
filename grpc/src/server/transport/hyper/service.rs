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

use std::convert::Infallible;
use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use http::HeaderValue;
use http::Request;
use http::Response;
use http::header::CONTENT_TYPE;
use http_body::Body;
use hyper::service::Service as HyperService;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::core::ConnectionInfo;
use crate::metadata::KeyAndValueRef;
use crate::rt::BoxFuture;
use crate::rt::GrpcRuntime;
use crate::rt::Sleep;
use crate::server::CallOptions;
use crate::server::DynHandle;
use crate::server::ResponseHeaders;
use crate::server::Trailers;
use crate::status::StatusCodeError;
use crate::status::StatusError;

use super::stream::HyperServerRecvStream;
use super::stream::HyperServerSendStream;
use super::stream::InitialResponse;
use super::stream::ServerResponseBody;
use super::stream::trailers_only_response;
use super::validation::extract_request_headers;
use super::validation::is_reserved_header;

/// Capacity (in messages) of the channel between the handler's [`HyperServerSendStream`]
/// and the streaming [`ServerResponseBody`].
///
/// Matches the client transport's request message channel; buffering beyond a
/// single message is left to the HTTP/2 send buffer and flow control.
// TODO: Revisit the size.
const RESPONSE_MESSAGE_CHANNEL_CAPACITY: usize = 1;

/// Hyper [`Service`](HyperService) adapter that dispatches incoming HTTP/2 requests to a [`DynHandle`].
pub struct HyperGrpcService {
    handler: Arc<dyn DynHandle>,
    runtime: GrpcRuntime,
    max_recv_message_size: Option<usize>,
    connection_info: ConnectionInfo,
}

impl HyperGrpcService {
    /// Creates a service that dispatches the requests of one connection to `handler`.
    pub fn new(
        handler: Arc<dyn DynHandle>,
        runtime: GrpcRuntime,
        max_recv_message_size: Option<usize>,
        connection_info: ConnectionInfo,
    ) -> Self {
        Self {
            handler,
            runtime,
            max_recv_message_size,
            connection_info,
        }
    }
}

impl<B> HyperService<Request<B>> for HyperGrpcService
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    type Response = Response<ServerResponseBody>;
    type Error = Infallible;
    // TODO: Replace `BoxFuture` with a concrete unboxed future (or `impl_trait_in_assoc_type`
    // once stabilized) to avoid boxing the response future on every RPC.
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn call(&self, req: Request<B>) -> Self::Future {
        let handler = self.handler.clone();
        let runtime = self.runtime.clone();
        let max_recv_message_size = self.max_recv_message_size;
        let connection_info = self.connection_info.clone();

        Box::pin(async move {
            // 1. Validate request and extract gRPC request headers.
            let (parts, body) = req.into_parts();
            let request_headers = match extract_request_headers(&parts, connection_info) {
                Ok(h) => h,
                Err(err) => return Ok(err.into_http()),
            };

            // 2. Create channels.
            let (initial_tx, initial_rx) = oneshot::channel::<InitialResponse>();
            let (body_tx, body_rx) = mpsc::channel(RESPONSE_MESSAGE_CHANNEL_CAPACITY);

            // 3. Create streams.
            let mut tx = HyperServerSendStream::new(initial_tx, body_tx);
            let rx = HyperServerRecvStream::new(body, max_recv_message_size);

            // 4. Spawn handler on runtime and retain its TaskHandle so that
            // stream cancellation (`RST_STREAM`) or connection teardown aborts it.
            // TODO: Move deadline enforcement to the transport-agnostic Server layer.
            let deadline = request_headers.timeout().map(|t| runtime.sleep(t));
            // TODO: Handle mid-stream handler panics (panicking after `ResponseHeaders` are
            // sent drops `tx` without calling `send_trailers`, closing `body_rx` cleanly so
            // `EncodeBody` emits `grpc-status: 0`).
            let task_handle = runtime.spawn(Box::pin(async move {
                let options = CallOptions::default();
                let trailers = with_deadline(
                    handler.dyn_handle(request_headers, options, &mut tx, Box::new(rx)),
                    deadline,
                )
                .await;
                // TODO: Consider overriding the handler-returned status (or surfacing a rich
                // status through `RecvStream::next`) when a transport/framing receive error
                // (e.g. `RESOURCE_EXHAUSTED` on `max_recv_message_size` exceeded) occurs.
                tx.send_trailers(trailers).await;
            }));
            // Built before awaiting `initial_rx` so that dropping this future aborts the handler.
            let body = ServerResponseBody::streaming(body_rx, task_handle);

            // 5. Await initial response headers (or trailers-only status) from the handler.
            match initial_rx.await {
                Ok(InitialResponse::Headers(response_headers)) => {
                    Ok(streaming_response(response_headers, body))
                }
                Ok(InitialResponse::TrailersOnly(trailers)) => Ok(trailers_only_response(trailers)),
                Err(_) => Ok(trailers_only_response(Trailers::new(Err(
                    StatusError::new(
                        StatusCodeError::Internal,
                        "handler did not send response headers or trailers",
                    ),
                )))),
            }
        })
    }
}

/// Builds a streaming HTTP/2 [`Response`] from the handler's initial [`ResponseHeaders`]
/// and [`ServerResponseBody`], stripping any reserved HTTP/2 or gRPC headers.
fn streaming_response(
    mut response_headers: ResponseHeaders,
    body: ServerResponseBody,
) -> Response<ServerResponseBody> {
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    // Reserved headers belong to the transport. A handler that sets `grpc-status`
    // here causes the client to treat the response as trailers-only, dropping all
    // response messages.
    response_headers.metadata_mut().retain(|entry| match entry {
        KeyAndValueRef::Ascii(key, _) => !is_reserved_header(key.as_str()),
        KeyAndValueRef::Binary(key, _) => !is_reserved_header(key.as_str()),
    });
    headers.extend(response_headers.into_metadata().into_headers());
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));

    response
}

/// Runs `handle`, or returns `DEADLINE_EXCEEDED` trailers if `deadline` elapses first.
async fn with_deadline(
    handle: impl Future<Output = Trailers>,
    deadline: Option<Pin<Box<dyn Sleep>>>,
) -> Trailers {
    let Some(deadline) = deadline else {
        return handle.await;
    };
    tokio::select! {
        biased;
        () = deadline => Trailers::new(Err(StatusError::new(
            StatusCodeError::DeadlineExceeded,
            "deadline exceeded",
        ))),
        trailers = handle => trailers,
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use bytes::Bytes;
    use http::Method;
    use http::Request;
    use http::StatusCode;
    use http_body::Body;
    use http_body_util::BodyExt;
    use http_body_util::Empty;
    use hyper::service::Service as HyperService;
    use tokio::time::timeout;

    use super::*;
    use crate::attributes::Attributes;
    use crate::core::Address;
    use crate::credentials::SecurityInfo;
    use crate::credentials::SecurityLevel;
    use crate::metadata::MetadataMap;
    use crate::metadata::MetadataValue;
    use crate::rt::default_runtime;
    use crate::server::Handle;
    use crate::server::RecvStream;
    use crate::server::RequestHeaders;
    use crate::server::ResponseHeaders;
    use crate::server::ResponseStreamItem;
    use crate::server::SendOptions;
    use crate::server::SendStream;
    use crate::server::Trailers;
    use crate::server::transport::hyper::test::DummyHandle;
    use crate::server::transport::hyper::test::PendingStreamHandle;
    use crate::server::transport::hyper::test::TestSendMessage;
    use crate::server::transport::hyper::test::valid_grpc_request;
    use crate::status::StatusCodeError;
    use crate::status::StatusError;

    fn test_grpc_service(handler: Arc<dyn DynHandle>) -> HyperGrpcService {
        let addr = Address {
            network_type: "tcp",
            address: "127.0.0.1:50051".to_string().into(),
            attributes: Attributes::new(),
        };
        let connection_info = ConnectionInfo::new(
            addr.clone(),
            addr,
            SecurityInfo::new("local").with_security_level(SecurityLevel::NoSecurity),
        );
        HyperGrpcService {
            handler,
            runtime: default_runtime(),
            max_recv_message_size: Some(1024),
            connection_info,
        }
    }

    fn grpc_request_with_timeout(timeout: &'static str) -> Request<Empty<Bytes>> {
        let mut req = valid_grpc_request();
        req.headers_mut()
            .insert("grpc-timeout", HeaderValue::from_static(timeout));
        req
    }

    /// Never sends headers and never finishes.
    struct NeverRespondsHandle;

    impl Handle for NeverRespondsHandle {
        async fn handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            _tx: &mut impl SendStream,
            _rx: impl RecvStream + 'static,
        ) -> Trailers {
            pending::<Trailers>().await
        }
    }

    #[tokio::test]
    async fn grpc_service_rejects_get_with_method_not_allowed() {
        let service = test_grpc_service(Arc::new(DummyHandle));
        let bad_req = Request::builder()
            .method(Method::GET)
            .uri("http://localhost/test.Service/Unary")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let resp = service.call(bad_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(resp.body().is_end_stream());
    }

    #[tokio::test]
    async fn grpc_service_streaming_response_strips_reserved_headers() {
        struct StreamingWithHeadersHandle;

        impl Handle for StreamingWithHeadersHandle {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                let mut meta = MetadataMap::new();
                meta.insert("x-custom", MetadataValue::from_static("allowed"));
                meta.insert_bin("x-custom-bin", MetadataValue::from_bytes(b"\x01\x02"));
                meta.insert("grpc-status", MetadataValue::from_static("0"));
                meta.insert("te", MetadataValue::from_static("trailers"));
                meta.insert_bin(
                    "grpc-status-details-bin",
                    MetadataValue::from_bytes(b"\x00"),
                );

                let headers = ResponseHeaders::new().with_metadata(meta);
                tx.send(ResponseStreamItem::Headers(headers), SendOptions::default())
                    .await
                    .unwrap();
                let msg = TestSendMessage::new(Bytes::from_static(b"hi"));
                tx.send(ResponseStreamItem::Message(&msg), SendOptions::default())
                    .await
                    .unwrap();
                Trailers::new(Ok(()))
            }
        }

        let service = test_grpc_service(Arc::new(StreamingWithHeadersHandle));
        let resp = service.call(valid_grpc_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/grpc"
        );
        assert_eq!(resp.headers().get("x-custom").unwrap(), "allowed");
        assert_eq!(resp.headers().get("x-custom-bin").unwrap(), "AQI");
        assert!(resp.headers().get("grpc-status").is_none());
        assert!(resp.headers().get("te").is_none());
        assert_eq!(resp.headers().get("grpc-status-details-bin").unwrap(), "AA");
        assert!(!resp.body().is_end_stream());

        let collected = resp.into_body().collect().await.unwrap();
        let trailers = collected.trailers().expect("expected trailers frame");
        assert_eq!(trailers.get("grpc-status").unwrap(), "0");
    }

    #[tokio::test]
    async fn grpc_service_returns_trailers_only_when_handler_skips_headers() {
        struct TrailersOnlyHandle;

        impl Handle for TrailersOnlyHandle {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                _tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                Trailers::new(Err(StatusError::new(
                    StatusCodeError::Unauthenticated,
                    "missing credentials",
                )))
            }
        }

        let service = test_grpc_service(Arc::new(TrailersOnlyHandle));
        let resp = service.call(valid_grpc_request()).await.unwrap();
        assert_eq!(resp.headers().get("grpc-status").unwrap(), "16");
        assert_eq!(
            resp.headers().get("grpc-message").unwrap(),
            "missing%20credentials"
        );
        assert!(resp.body().is_end_stream());
    }

    #[tokio::test]
    async fn grpc_service_returns_internal_when_handler_panics_before_headers() {
        struct PanicBeforeHeadersHandle;

        impl Handle for PanicBeforeHeadersHandle {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                _tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                panic!("handler panicked before sending headers");
            }
        }

        let service = test_grpc_service(Arc::new(PanicBeforeHeadersHandle));
        let resp = service.call(valid_grpc_request()).await.unwrap();
        assert_eq!(resp.headers().get("grpc-status").unwrap(), "13");
        assert_eq!(
            resp.headers().get("grpc-message").unwrap(),
            "handler%20did%20not%20send%20response%20headers%20or%20trailers"
        );
        assert!(resp.body().is_end_stream());
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_exceeded_before_headers_returns_trailers_only() {
        let service = test_grpc_service(Arc::new(NeverRespondsHandle));

        let resp = service.call(grpc_request_with_timeout("1S")).await.unwrap();

        assert_eq!(resp.headers().get("grpc-status").unwrap(), "4");
        assert_eq!(
            resp.headers().get("grpc-message").unwrap(),
            "deadline%20exceeded"
        );
        assert!(resp.body().is_end_stream());
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_exceeded_after_headers_sends_status_in_trailers() {
        let service = test_grpc_service(Arc::new(PendingStreamHandle));

        let resp = service.call(grpc_request_with_timeout("1S")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let collected = resp.into_body().collect().await.unwrap();

        let trailers = collected.trailers().expect("expected trailers frame");
        assert_eq!(trailers.get("grpc-status").unwrap(), "4");
    }

    #[tokio::test(start_paused = true)]
    async fn handler_finishing_before_deadline_keeps_its_status() {
        let service = test_grpc_service(Arc::new(DummyHandle));

        let resp = service.call(grpc_request_with_timeout("1S")).await.unwrap();

        assert_eq!(resp.headers().get("grpc-status").unwrap(), "0");
    }

    #[tokio::test(start_paused = true)]
    async fn missing_grpc_timeout_does_not_enforce_a_deadline() {
        let service = test_grpc_service(Arc::new(NeverRespondsHandle));

        assert!(
            timeout(
                Duration::from_secs(24 * 60 * 60),
                service.call(valid_grpc_request())
            )
            .await
            .is_err(),
            "call completed for a request without grpc-timeout"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_exceeded_drops_the_handler_future() {
        struct DropFlag(Arc<AtomicBool>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        struct FlagOnDropHandle(Arc<AtomicBool>);

        impl Handle for FlagOnDropHandle {
            async fn handle(
                &self,
                _headers: RequestHeaders,
                _options: CallOptions,
                _tx: &mut impl SendStream,
                _rx: impl RecvStream + 'static,
            ) -> Trailers {
                let _flag = DropFlag(self.0.clone());
                pending::<Trailers>().await
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let service = test_grpc_service(Arc::new(FlagOnDropHandle(dropped.clone())));

        let resp = service.call(grpc_request_with_timeout("1S")).await.unwrap();

        assert_eq!(resp.headers().get("grpc-status").unwrap(), "4");
        assert!(dropped.load(Ordering::SeqCst));
    }
}
