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

use core::panic;
use std::fmt::Debug;
use std::fmt::Display;
use std::hash::Hash;
use std::ptr::addr_eq;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::Notify;
use tokio::sync::oneshot;
use tonic::async_trait;

use crate::StatusCodeError;
use crate::StatusError;
use crate::client::CallOptions;
use crate::client::ConnectivityState;
use crate::client::DynInvoke;
use crate::client::DynRecvStream;
use crate::client::DynSendStream;
use crate::client::channel::WorkQueueItem;
use crate::client::channel::WorkQueueTx;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::load_balancing::subchannel::private::Sealed;
use crate::client::name_resolution::Address;
use crate::client::stream_util::FailingRecvStream;
use crate::client::transport::DynTransport;
use crate::client::transport::SecurityOpts;
use crate::client::transport::TransportOptions;
use crate::core::RequestHeaders;
use crate::credentials::call::CallDetails;
use crate::credentials::call::ClientConnectionSecurityInfo as CallClientConnectionSecurityInfo;
use crate::credentials::client::ClientConnectionSecurityContext;
use crate::credentials::client::ClientConnectionSecurityInfo;
use crate::credentials::common::Authority;
use crate::rt::GrpcRuntime;

type SharedInvoke = Arc<dyn DynInvoke>;

pub trait Backoff: Send + Sync {
    fn backoff_until(&self) -> Instant;
    fn reset(&self);
    fn min_connect_timeout(&self) -> Duration;
}

// TODO(easwars): Move this somewhere else, where appropriate.
pub(crate) struct NopBackoff {}
impl Backoff for NopBackoff {
    fn backoff_until(&self) -> Instant {
        Instant::now()
    }
    fn reset(&self) {}
    fn min_connect_timeout(&self) -> Duration {
        Duration::from_secs(20)
    }
}

struct ReadyState {
    service: Box<dyn DynInvoke>,
    security_info: ClientConnectionSecurityInfo<Box<dyn ClientConnectionSecurityContext>>,
    authority: Authority,
}

enum InternalSubchannelState {
    Idle,
    Connecting,
    Ready(Arc<ReadyState>),
    TransientFailure(String),
}

impl<'a> From<&'a InternalSubchannelState> for SubchannelState {
    fn from(iss: &'a InternalSubchannelState) -> SubchannelState {
        match &iss {
            InternalSubchannelState::Idle => SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
            InternalSubchannelState::Connecting => SubchannelState {
                connectivity_state: ConnectivityState::Connecting,
                last_connection_error: None,
            },
            InternalSubchannelState::Ready(_) => SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
            InternalSubchannelState::TransientFailure(err) => SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some(err.clone()),
            },
        }
    }
}

impl Display for InternalSubchannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Connecting => write!(f, "Connecting"),
            Self::Ready(_) => write!(f, "Ready"),
            Self::TransientFailure(_) => write!(f, "TransientFailure"),
        }
    }
}

impl Debug for InternalSubchannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Connecting => write!(f, "Connecting"),
            Self::Ready(_) => write!(f, "Ready"),
            Self::TransientFailure(_) => write!(f, "TransientFailure"),
        }
    }
}

impl PartialEq for InternalSubchannelState {
    fn eq(&self, other: &Self) -> bool {
        match &self {
            Self::Idle => {
                if let Self::Idle = other {
                    return true;
                }
            }
            Self::Connecting => {
                if let Self::Connecting = other {
                    return true;
                }
            }
            Self::Ready(_) => {
                if let Self::Ready(_) = other {
                    return true;
                }
            }
            Self::TransientFailure(_) => {
                if let Self::TransientFailure(_) = other {
                    return true;
                }
            }
        }
        false
    }
}

#[async_trait]
impl DynInvoke for InternalSubchannel {
    async fn dyn_invoke(
        &self,
        mut headers: RequestHeaders,
        options: CallOptions,
    ) -> (Box<dyn DynSendStream>, Box<dyn DynRecvStream>) {
        let (state, call_creds) = {
            let data = self.data.lock().unwrap();

            let state = match &data.state {
                InternalSubchannelState::Ready(state) => state.clone(),
                _ => todo!("handle non-READY subchannel"),
            };

            let creds = data
                .security_opts
                .credentials
                .get_call_credentials()
                .cloned();

            (state, creds)
        };

        let fail_with = |status| -> (Box<dyn DynSendStream>, Box<dyn DynRecvStream>) {
            FailingRecvStream::new_stream_pair(status)
        };

        if let Some(call_creds) = call_creds {
            if call_creds.minimum_channel_security_level() > state.security_info.security_level() {
                return fail_with(StatusError::new(
                    StatusCodeError::Unauthenticated,
                    "transport: cannot send secure credentials on an insecure connection",
                ));
            }

            let call_details = create_call_details(&state.authority, headers.method_name());

            let channel_sec_info = CallClientConnectionSecurityInfo::new(
                state.security_info.security_protocol(),
                state.security_info.security_level(),
                state.security_info.attributes().clone(),
            );

            if let Err(s) = call_creds
                .get_metadata(&call_details, &channel_sec_info, headers.metadata_mut())
                .await
            {
                let status = if s.is_restricted_control_plane_code() {
                    StatusError::new(
                        StatusCodeError::Internal,
                        format!(
                            "transport: received call credentials error with illegal status: {}",
                            s.message()
                        ),
                    )
                } else {
                    s
                };

                return fail_with(status);
            }
        }

        state.service.dyn_invoke(headers, options).await
    }
}

pub(crate) struct InternalSubchannel {
    address: Address,
    data: Arc<Mutex<InternalSubchannelData>>,
    on_drop: Arc<Notify>,
}

struct InternalSubchannelData {
    address: String,
    state: InternalSubchannelState,
    work_queue: WorkQueueTx,
    on_drop: Arc<Notify>,
    transport_builder: Arc<dyn DynTransport>,
    backoff: Arc<dyn Backoff>,
    runtime: GrpcRuntime,
    transport_options: TransportOptions,
    security_opts: SecurityOpts,
    weak_self: Weak<InternalSubchannel>,
}

impl InternalSubchannelData {
    fn update_state(&mut self, state: InternalSubchannelState) {
        self.state = state;
        let state: SubchannelState = (&self.state).into();

        let Some(subchannel) = self.weak_self.upgrade() else {
            return;
        };

        _ = self
            .work_queue
            .send(WorkQueueItem::SubchannelStateUpdate { subchannel, state });
    }
}

impl Sealed for InternalSubchannel {}

impl Subchannel for InternalSubchannel {
    fn address(&self) -> Address {
        self.address.clone()
    }

    fn connect(&self) {
        begin_connecting_if_idle(self.data.clone());
    }
}

impl Eq for InternalSubchannel {}

impl Hash for InternalSubchannel {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for InternalSubchannel {
    fn eq(&self, other: &Self) -> bool {
        addr_eq(self, other)
    }
}

impl InternalSubchannel {
    pub(super) fn new_arc(
        address: Address,
        transport: Arc<dyn DynTransport>,
        backoff: Arc<dyn Backoff>,
        runtime: GrpcRuntime,
        security_opts: SecurityOpts,
        work_queue: WorkQueueTx,
    ) -> Arc<dyn Subchannel> {
        let on_drop = Arc::new(Notify::new());
        let address_string = address.address.to_string();
        let this = Arc::new_cyclic(|weak_self| Self {
            address,
            on_drop: on_drop.clone(),
            data: Arc::new(Mutex::new(InternalSubchannelData {
                address: address_string,
                transport_builder: transport,
                backoff,
                weak_self: weak_self.clone(),
                runtime,
                state: InternalSubchannelState::Idle,
                work_queue,
                on_drop,
                transport_options: TransportOptions::default(), // TODO: should be configurable
                security_opts,
            })),
        });
        move_to_idle(&this.data);
        this
    }

    pub(super) fn address(&self) -> Address {
        self.address.clone()
    }
}

fn move_to_idle(data: &Arc<Mutex<InternalSubchannelData>>) {
    data.lock()
        .unwrap()
        .update_state(InternalSubchannelState::Idle);
}

// Starts connecting in the background and manages the full lifecycle of the
// subchannel until it returns back to idle in that background task.
fn begin_connecting_if_idle(data: Arc<Mutex<InternalSubchannelData>>) {
    let mut data_locked = data.lock().unwrap();
    if data_locked.state != InternalSubchannelState::Idle {
        return;
    }
    data_locked.update_state(InternalSubchannelState::Connecting);

    let connect_timeout = data_locked.backoff.min_connect_timeout();
    let transport_builder = data_locked.transport_builder.clone();
    let address = data_locked.address.clone();
    let runtime = data_locked.runtime.clone();
    let on_drop = data_locked.on_drop.clone();
    let transport_opts = data_locked.transport_options.clone();
    let security_opts = data_locked.security_opts.clone();
    drop(data_locked);

    runtime.clone().spawn(Box::pin(async move {
        tokio::select! {
            _ = runtime.sleep(connect_timeout) => {
                move_to_transient_failure(data, "connect timeout expired".into()).await;
            }
            _ = on_drop.notified() => {
            }
            result = transport_builder.dyn_connect(address, runtime, &security_opts, &transport_opts) => {
                    match result {
                        Ok((service, security_info, disconnection_listener)) => {
                            move_to_ready(data, Arc::new(ReadyState{
                                service,
                                security_info,
                                authority: security_opts.authority}), disconnection_listener).await;
                        }
                        Err(e) => {
                            move_to_transient_failure(data, e).await;
                        }
                    }
            },
        }
    }));
}

// Sets the state to ready and then waits until the subchannel is dropped or
// the connection is lost.  Moves to idle upon connection loss.
async fn move_to_ready(
    data: Arc<Mutex<InternalSubchannelData>>,
    svc: Arc<ReadyState>,
    closed_rx: oneshot::Receiver<Result<(), String>>,
) {
    let on_drop;
    {
        let mut data = data.lock().unwrap();
        // Reset connection backoff upon successfully moving to ready.
        data.backoff.reset();
        on_drop = data.on_drop.clone();
        data.update_state(InternalSubchannelState::Ready(svc.clone()));
    }
    // TODO(easwars): Does it make sense for disconnected() to return an
    // error string containing information about why the connection
    // terminated? But what can we do with that error other than logging
    // it, which the transport can do as well?
    tokio::select! {
        _ = on_drop.notified() => {}
        e = closed_rx => {
            eprintln!("Transport closed: {e:?}");
            move_to_idle(&data);
        }
    }
}

// Sets the state to transient failure and then waits until the subchannel
// is dropped or the backoff expires.  Moves to idle upon backoff expiry.
async fn move_to_transient_failure(data: Arc<Mutex<InternalSubchannelData>>, err: String) {
    let on_drop;
    let backoff_fut;
    {
        let mut data = data.lock().unwrap();
        on_drop = data.on_drop.clone();
        let backoff_interval = data.backoff.backoff_until();
        backoff_fut = data
            .runtime
            .sleep(backoff_interval.saturating_duration_since(Instant::now()));
        data.update_state(InternalSubchannelState::TransientFailure(err.clone()));
    }
    tokio::select! {
        _ = on_drop.notified() => {}
        _ = backoff_fut => {
            move_to_idle(&data);
        }
    }
}

impl Drop for InternalSubchannel {
    fn drop(&mut self) {
        self.on_drop.notify_waiters();
    }
}

fn create_call_details(authority: &Authority, full_method: &str) -> CallDetails {
    let (service, method) = full_method.rsplit_once('/').unwrap_or((full_method, ""));
    let host_str = authority.host();

    let host = if let Some(443) = authority.port() {
        host_str.to_string()
    } else {
        authority.host_port_string()
    };

    CallDetails::new(format!("https://{}{}", host, service), method.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_call_details() {
        let authority = Authority::new("localhost", None);
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://localhost/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("localhost", Some(50051));
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://localhost:50051/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("localhost", Some(443));
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://localhost/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("::1", Some(50051));
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://[::1]:50051/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("::1", None);
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://::1/service");
        assert_eq!(details.method_name(), "method");
    }
}
