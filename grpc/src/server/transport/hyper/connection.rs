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

use std::future::Future;
use std::ops::RangeInclusive;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::task::ready;
use std::time::Duration;

use hyper::server::conn::http2::Builder as Http2Builder;
use hyper::server::conn::http2::Connection as Http2Connection;
use rand::RngExt;

use crate::attributes::Attributes;
use crate::core::Address;
use crate::core::ConnectionInfo;
use crate::credentials::ServerCredentials;
use crate::credentials::server::HandshakeOutput;
use crate::private::Internal;
use crate::rt::BoxEndpoint;
use crate::rt::BoxFuture;
use crate::rt::GrpcRuntime;
use crate::rt::Sleep;
use crate::rt::hyper_wrapper::HyperCompatExec;
use crate::rt::hyper_wrapper::HyperCompatTimer;
use crate::rt::hyper_wrapper::HyperStream;
use crate::server::DynHandle;
use crate::server::GracefulConnection;
use crate::server::Transport;

use super::config::Http2Config;
use super::service::HyperGrpcService;

/// Concrete Hyper HTTP/2 server connection type used once the credential handshake completes.
type HyperH2Connection = Http2Connection<HyperStream, HyperGrpcService, HyperCompatExec>;

/// Range of the factor applied to `max_connection_age`: ±10% per gRFC A9.
const CONNECTION_AGE_JITTER_RANGE: RangeInclusive<f64> = 0.9..=1.1;

/// Draws a `max_connection_age` factor uniformly from `CONNECTION_AGE_JITTER_RANGE`.
fn random_age_jitter() -> f64 {
    rand::rng().random_range(CONNECTION_AGE_JITTER_RANGE)
}

/// Scales `age` by `factor`, or returns `age` unchanged if the result can't be represented.
fn jittered_age(age: Duration, factor: f64) -> Duration {
    Duration::try_from_secs_f64(age.as_secs_f64() * factor).unwrap_or(age)
}

/// An accepted raw endpoint together with its server credentials and HTTP/2 configuration,
/// ready to be served as an HTTP/2 connection via [`Transport::serve`].
pub struct HyperTransport {
    io: BoxEndpoint,
    creds: Arc<dyn ServerCredentials>,
    config: Http2Config,
    /// Factor applied to `max_connection_age` for this connection.
    age_jitter: f64,
}

impl HyperTransport {
    /// Creates a new [`HyperTransport`] for an accepted endpoint.
    pub(crate) fn new(
        io: BoxEndpoint,
        creds: Arc<dyn ServerCredentials>,
        config: Http2Config,
    ) -> Self {
        let age_jitter = match config.get_max_connection_age() {
            Some(_) => random_age_jitter(),
            None => 1.0,
        };
        Self::with_age_jitter(io, creds, config, age_jitter)
    }

    /// Like [`HyperTransport::new`], but with a fixed `max_connection_age`
    /// jitter factor.
    fn with_age_jitter(
        io: BoxEndpoint,
        creds: Arc<dyn ServerCredentials>,
        config: Http2Config,
        age_jitter: f64,
    ) -> Self {
        Self {
            io,
            creds,
            config,
            age_jitter,
        }
    }
}

impl Transport for HyperTransport {
    type Connection = HyperServingConnection;

    fn serve(
        self,
        handler: Arc<dyn DynHandle>,
        runtime: GrpcRuntime,
        _token: Internal,
    ) -> HyperServingConnection {
        HyperServingConnection::new(
            self.io,
            self.creds,
            self.config,
            self.age_jitter,
            handler,
            runtime,
        )
    }
}

/// Lifecycle phase of a [`HyperServingConnection`].
enum ServingPhase {
    /// Performing the initial credential handshake before starting HTTP/2.
    Handshaking {
        handshake_fut: BoxFuture<Result<HandshakeOutput, String>>,
        handshake_timer: Pin<Box<dyn Sleep>>,
        handler: Arc<dyn DynHandle>,
        shutdown_requested: bool,
    },
    /// Actively serving HTTP/2 RPC streams over the established connection.
    ///
    /// Boxed because hyper's connection is much larger than the handshake state.
    Serving(Box<HyperH2Connection>),
}

impl ServingPhase {
    /// Signals graceful shutdown on the active phase: closing the connection on its next poll
    /// if still handshaking, or sending an HTTP/2 `GOAWAY` frame if already serving.
    fn initiate_graceful_shutdown(&mut self) {
        match self {
            ServingPhase::Handshaking {
                shutdown_requested, ..
            } => {
                *shutdown_requested = true;
            }
            ServingPhase::Serving(conn) => {
                Pin::new(conn.as_mut()).graceful_shutdown();
            }
        }
    }
}

/// State machine tracking `max_connection_age` and `max_connection_age_grace` timers.
enum ConnectionAgeState {
    /// No `max_connection_age` is configured.
    Unlimited,
    /// Waiting for `max_connection_age` to elapse before initiating graceful shutdown.
    Aging(Pin<Box<dyn Sleep>>),
    /// `max_connection_age` elapsed; waiting for `max_connection_age_grace` before force-closing.
    GracePeriod(Pin<Box<dyn Sleep>>),
    /// `max_connection_age` elapsed with no grace timeout configured; draining in-flight RPCs indefinitely.
    DrainingIndefinitely,
}

/// Default timeout for the initial credential handshake when not overridden in [`Http2Config`].
const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(120);

/// A serving HTTP/2 connection.
///
/// Drives the initial server credential handshake (`ServingPhase::Handshaking`)
/// before transitioning to hyper's HTTP/2 `Connection` (`ServingPhase::Serving`),
/// and enforces `max_connection_age` / `max_connection_age_grace` timers (gRFC A9).
pub struct HyperServingConnection {
    phase: ServingPhase,
    connection_age_state: ConnectionAgeState,
    config: Http2Config,
    runtime: GrpcRuntime,
}

impl HyperServingConnection {
    fn new(
        io: BoxEndpoint,
        creds: Arc<dyn ServerCredentials>,
        config: Http2Config,
        age_jitter: f64,
        handler: Arc<dyn DynHandle>,
        runtime: GrpcRuntime,
    ) -> Self {
        let rt_clone = runtime.clone();
        let handshake_timeout = config
            .get_handshake_timeout()
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT);

        // TODO: Change `ServerCredentials::accept` to return a `'static` `BoxFuture`
        // so `creds.accept(...)` can be stored directly without double-boxing here.
        let handshake_fut = Box::pin(async move { creds.accept(io, rt_clone, Internal).await });
        // TODO: Also time out the wait for the client's HTTP/2 preface. hyper currently
        // has no timeout or API to wait for it, and graceful shutdown can't interrupt it.
        let handshake_timer = runtime.sleep(handshake_timeout);

        let connection_age_state = match config.get_max_connection_age() {
            Some(age) => ConnectionAgeState::Aging(runtime.sleep(jittered_age(age, age_jitter))),
            None => ConnectionAgeState::Unlimited,
        };

        Self {
            phase: ServingPhase::Handshaking {
                handshake_fut,
                handshake_timer,
                handler,
                shutdown_requested: false,
            },
            connection_age_state,
            config,
            runtime,
        }
    }

    /// Constructs a configured Hyper HTTP/2 server connection from a completed credential handshake.
    fn build_h2_connection(
        handshake: HandshakeOutput,
        handler: Arc<dyn DynHandle>,
        config: &Http2Config,
        runtime: &GrpcRuntime,
    ) -> HyperH2Connection {
        let endpoint = handshake.endpoint;
        let network_type = endpoint.get_network_type();
        let local_address = Address {
            network_type,
            address: endpoint.get_local_address().to_string().into(),
            attributes: Attributes::new(),
        };
        let remote_address = Address {
            network_type,
            address: endpoint.get_peer_address().to_string().into(),
            attributes: Attributes::new(),
        };
        let connection_info =
            ConnectionInfo::new(local_address, remote_address, handshake.security);

        let service = HyperGrpcService::new(
            handler,
            runtime.clone(),
            config.get_max_recv_message_size(),
            connection_info,
        );

        // Apply HTTP/2 settings from config to the Hyper connection builder.
        let mut h2_builder = Http2Builder::new(HyperCompatExec {
            inner: runtime.clone(),
        });
        h2_builder
            .timer(HyperCompatTimer {
                inner: runtime.clone(),
            })
            .max_concurrent_streams(config.get_max_concurrent_streams())
            .initial_connection_window_size(config.get_initial_connection_window_size())
            .initial_stream_window_size(config.get_initial_stream_window_size())
            .keep_alive_interval(config.get_keep_alive_interval());
        if let Some(timeout) = config.get_keep_alive_timeout() {
            h2_builder.keep_alive_timeout(timeout);
        }

        h2_builder.serve_connection(HyperStream::new(endpoint), service)
    }
}

impl Future for HyperServingConnection {
    type Output = Result<(), String>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), String>> {
        let this = self.get_mut();

        // 1. Check max_connection_age timer across both Handshaking and Serving phases.
        if let ConnectionAgeState::Aging(timer) = &mut this.connection_age_state
            && let Poll::Ready(()) = timer.as_mut().poll(cx)
        {
            this.phase.initiate_graceful_shutdown();
            this.connection_age_state = match this.config.get_max_connection_age_grace() {
                Some(grace) => ConnectionAgeState::GracePeriod(this.runtime.sleep(grace)),
                None => ConnectionAgeState::DrainingIndefinitely,
            };
        }

        // 2. Grace timer expired? -> force close with error.
        if let ConnectionAgeState::GracePeriod(timer) = &mut this.connection_age_state
            && let Poll::Ready(()) = timer.as_mut().poll(cx)
        {
            return Poll::Ready(Err(
                "connection force-closed: max_connection_age_grace expired".to_string(),
            ));
        }

        // 3. Drive Handshaking phase if not yet transitioned to Serving.
        if let ServingPhase::Handshaking {
            handshake_fut,
            handshake_timer,
            handler,
            shutdown_requested,
        } = &mut this.phase
        {
            if *shutdown_requested {
                return Poll::Ready(Ok(()));
            }
            if handshake_timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err("handshake timed out".to_string()));
            }
            let handshake = ready!(handshake_fut.as_mut().poll(cx))?;
            let conn =
                Self::build_h2_connection(handshake, handler.clone(), &this.config, &this.runtime);
            this.phase = ServingPhase::Serving(Box::new(conn));
        }

        // 4. Drive Serving phase.
        if let ServingPhase::Serving(conn) = &mut this.phase
            && let Poll::Ready(res) = Pin::new(conn.as_mut()).poll(cx)
        {
            return Poll::Ready(res.map_err(|err| err.to_string()));
        }

        Poll::Pending
    }
}

impl GracefulConnection for HyperServingConnection {
    fn graceful_shutdown(self: Pin<&mut Self>, _token: Internal) {
        self.get_mut().phase.initiate_graceful_shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::future::poll_fn;
    use std::pin::pin;
    use std::task::Poll;
    use std::time::Duration;

    use bytes::Bytes;
    use http_body_util::BodyExt;
    use http_body_util::Empty;
    use hyper::client::conn::http2::Builder as ClientHttp2Builder;
    use tokio::sync::oneshot;
    use tokio::time::sleep;
    use tokio::time::timeout;

    use super::*;
    use crate::credentials::ProtocolInfo;
    use crate::rt::default_runtime;
    use crate::server::CallOptions;
    use crate::server::Handle;
    use crate::server::RecvStream;
    use crate::server::RequestHeaders;
    use crate::server::ResponseHeaders;
    use crate::server::ResponseStreamItem;
    use crate::server::SendOptions;
    use crate::server::SendStream;
    use crate::server::Trailers;
    use crate::server::transport::hyper::test::DummyHandle;
    use crate::server::transport::hyper::test::FINISH_TIMEOUT;
    use crate::server::transport::hyper::test::InsecureTestCreds;
    use crate::server::transport::hyper::test::PendingStreamHandle;
    use crate::server::transport::hyper::test::endpoint_pair;
    use crate::server::transport::hyper::test::valid_grpc_request;

    enum HandshakeBehavior {
        Fail,
        Stall,
    }

    struct StubHandshakeCreds(HandshakeBehavior);

    #[crate::async_trait]
    impl ServerCredentials for StubHandshakeCreds {
        fn info(&self) -> &ProtocolInfo {
            static INFO: ProtocolInfo = ProtocolInfo::new("stub");
            &INFO
        }

        async fn accept(
            &self,
            _source: BoxEndpoint,
            _runtime: GrpcRuntime,
            _token: Internal,
        ) -> Result<HandshakeOutput, String> {
            match self.0 {
                HandshakeBehavior::Fail => Err("simulated handshake error".to_string()),
                HandshakeBehavior::Stall => pending::<Result<HandshakeOutput, String>>().await,
            }
        }
    }

    /// Sends headers, then finishes with OK after `self.0`.
    struct SlowHandle(Duration);

    impl Handle for SlowHandle {
        async fn handle(
            &self,
            _headers: RequestHeaders,
            _options: CallOptions,
            tx: &mut impl SendStream,
            _rx: impl RecvStream + 'static,
        ) -> Trailers {
            tx.send(
                ResponseStreamItem::Headers(ResponseHeaders::default()),
                SendOptions::default(),
            )
            .await
            .unwrap();
            sleep(self.0).await;
            Trailers::new(Ok(()))
        }
    }

    #[tokio::test]
    async fn handshake_failure_propagates_on_serving_future() {
        let rt = default_runtime();
        let (server_ep, _client_ep) = endpoint_pair();
        let creds = Arc::new(StubHandshakeCreds(HandshakeBehavior::Fail));
        let transport = HyperTransport::new(server_ep, creds, Http2Config::new());
        let serving = transport.serve(Arc::new(DummyHandle), rt, Internal);
        assert_eq!(serving.await, Err("simulated handshake error".to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_timeout_propagates_on_serving_future() {
        let rt = default_runtime();
        let (server_ep, _client_ep) = endpoint_pair();
        let creds = Arc::new(StubHandshakeCreds(HandshakeBehavior::Stall));
        let config = Http2Config::new().handshake_timeout(Duration::from_millis(30));
        let transport = HyperTransport::new(server_ep, creds, config);
        let serving = transport.serve(Arc::new(DummyHandle), rt, Internal);
        assert_eq!(serving.await, Err("handshake timed out".to_string()));
    }

    #[tokio::test]
    async fn graceful_shutdown_during_handshake_resolves_cleanly() {
        let rt = default_runtime();
        let (server_ep, _client_ep) = endpoint_pair();
        let creds = Arc::new(StubHandshakeCreds(HandshakeBehavior::Stall));
        let transport = HyperTransport::new(server_ep, creds, Http2Config::new());
        let mut serving = pin!(transport.serve(Arc::new(DummyHandle), rt, Internal));

        poll_fn(|cx| {
            assert_eq!(serving.as_mut().poll(cx), Poll::Pending);
            Poll::Ready(())
        })
        .await;

        serving.as_mut().graceful_shutdown(Internal);
        assert_eq!(serving.await, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn graceful_shutdown_lets_in_flight_rpc_finish() {
        let rt = default_runtime();
        let (server_ep, client_ep) = endpoint_pair();
        let creds = Arc::new(InsecureTestCreds);
        let handler = Arc::new(SlowHandle(Duration::from_secs(60)));

        let transport = HyperTransport::new(server_ep, creds, Http2Config::new());
        let serving = transport.serve(handler, rt.clone(), Internal);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            let mut serving = pin!(serving);
            tokio::select! {
                outcome = serving.as_mut() => return outcome,
                _ = shutdown_rx => {}
            }
            serving.as_mut().graceful_shutdown(Internal);
            serving.await
        });

        let (mut send_req, client_conn) =
            ClientHttp2Builder::new(HyperCompatExec { inner: rt.clone() })
                .handshake::<_, Empty<Bytes>>(HyperStream::new(client_ep))
                .await
                .unwrap();
        let client_task = tokio::spawn(client_conn);
        let resp = send_req.send_request(valid_grpc_request()).await.unwrap();

        shutdown_tx.send(()).unwrap();
        sleep(Duration::from_secs(30)).await;
        assert!(!server_task.is_finished());

        let collected = resp.into_body().collect().await.unwrap();
        let trailers = collected.trailers().expect("expected trailers frame");
        assert_eq!(trailers.get("grpc-status").unwrap(), "0");

        let outcome = timeout(FINISH_TIMEOUT, server_task)
            .await
            .expect("serving connection should close once the last RPC finishes")
            .unwrap();
        assert_eq!(outcome, Ok(()));
        drop(send_req);
        timeout(FINISH_TIMEOUT, client_task)
            .await
            .expect("client connection should close")
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn connection_age_without_grace_lets_in_flight_rpc_finish() {
        let rt = default_runtime();
        let (server_ep, client_ep) = endpoint_pair();
        let creds = Arc::new(InsecureTestCreds);
        let config = Http2Config::new().max_connection_age(Duration::from_millis(30));
        let handler = Arc::new(SlowHandle(Duration::from_secs(60)));

        let transport = HyperTransport::with_age_jitter(server_ep, creds, config, 1.0);
        let serving = transport.serve(handler, rt.clone(), Internal);
        let server_task = tokio::spawn(serving);

        let (mut send_req, client_conn) =
            ClientHttp2Builder::new(HyperCompatExec { inner: rt.clone() })
                .handshake::<_, Empty<Bytes>>(HyperStream::new(client_ep))
                .await
                .unwrap();
        let client_task = tokio::spawn(client_conn);
        let resp = send_req.send_request(valid_grpc_request()).await.unwrap();

        // Well past max_connection_age (30ms), but before the handler finishes
        // at 60s: the connection must still be draining.
        sleep(Duration::from_secs(30)).await;
        assert!(!server_task.is_finished());

        let collected = resp.into_body().collect().await.unwrap();
        let trailers = collected.trailers().expect("expected trailers frame");
        assert_eq!(trailers.get("grpc-status").unwrap(), "0");

        let outcome = timeout(FINISH_TIMEOUT, server_task)
            .await
            .expect("serving connection should close once the last RPC finishes")
            .unwrap();
        assert_eq!(outcome, Ok(()));
        drop(send_req);
        timeout(FINISH_TIMEOUT, client_task)
            .await
            .expect("client connection should close")
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn connection_age_grace_period_force_closes_stalled_rpc() {
        let rt = default_runtime();
        let (server_ep, client_ep) = endpoint_pair();
        let creds = Arc::new(InsecureTestCreds);
        let config = Http2Config::new()
            .max_connection_age(Duration::from_millis(30))
            .max_connection_age_grace(Duration::from_millis(30));

        let transport = HyperTransport::with_age_jitter(server_ep, creds, config, 1.0);
        let serving = transport.serve(Arc::new(PendingStreamHandle), rt.clone(), Internal);
        let server_task = tokio::spawn(serving);

        let (mut send_req, client_conn) =
            ClientHttp2Builder::new(HyperCompatExec { inner: rt.clone() })
                .handshake::<_, Empty<Bytes>>(HyperStream::new(client_ep))
                .await
                .unwrap();
        let client_task = tokio::spawn(client_conn);

        // Start an in-flight RPC that never completes so the connection stays open
        // past max_connection_age and into max_connection_age_grace.
        let resp = send_req.send_request(valid_grpc_request()).await.unwrap();

        let outcome = timeout(FINISH_TIMEOUT, server_task)
            .await
            .expect("serving connection should force-close after grace period")
            .unwrap();
        assert_eq!(
            outcome,
            Err("connection force-closed: max_connection_age_grace expired".to_string())
        );

        // The client sees its in-flight RPC cut off.
        assert!(resp.into_body().collect().await.is_err());
        drop(send_req);
        timeout(FINISH_TIMEOUT, client_task)
            .await
            .expect("client connection should close")
            .unwrap()
            .unwrap();
    }

    #[test]
    fn jittered_age_scales_by_factor() {
        let age = Duration::from_secs(100);

        assert!(
            jittered_age(age, 0.9).abs_diff(Duration::from_secs(90)) < Duration::from_micros(1)
        );
        assert!(
            jittered_age(age, 1.1).abs_diff(Duration::from_secs(110)) < Duration::from_micros(1)
        );
    }

    #[test]
    fn jittered_age_unit_factor_is_identity() {
        let age = Duration::from_millis(1500);

        assert_eq!(jittered_age(age, 1.0), age);
    }

    #[test]
    fn jittered_age_falls_back_on_overflow() {
        assert_eq!(jittered_age(Duration::MAX, 1.1), Duration::MAX);
    }

    #[tokio::test(start_paused = true)]
    async fn connection_age_uses_injected_jitter() {
        let (server_ep, _client_ep) = endpoint_pair();
        let creds = Arc::new(StubHandshakeCreds(HandshakeBehavior::Stall));
        // A 200s age jittered by 0.5 fires at 100s, before the 120s default
        // handshake timeout; without the jitter the handshake would time out first.
        let config = Http2Config::new().max_connection_age(Duration::from_secs(200));

        let transport = HyperTransport::with_age_jitter(server_ep, creds, config, 0.5);
        let serving = transport.serve(Arc::new(DummyHandle), default_runtime(), Internal);

        assert_eq!(serving.await, Ok(()));
    }
}
