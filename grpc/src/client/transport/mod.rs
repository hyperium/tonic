use crate::{rt::Runtime, service::Service};
use std::time::Instant;
use std::{sync::Arc, time::Duration};

mod registry;

// Using tower/buffer enables tokio's rt feature even though it's possible to
// create Buffers with a user provided executor.
#[cfg(feature = "_runtime-tokio")]
mod tonic;

use ::tonic::async_trait;
pub(crate) use registry::TransportRegistry;
pub(crate) use registry::GLOBAL_TRANSPORT_REGISTRY;
use tokio::sync::oneshot;

pub(crate) struct ConnectedTransport {
    pub service: Box<dyn Service>,
    pub disconnection_listener: oneshot::Receiver<Result<(), String>>,
}

// TODO: The following options are specific to HTTP/2. We should
// instead pass an `Attribute` like struct to the connect method instead which
// can hold config relevant to a particular transport.
#[derive(Default)]
pub(crate) struct TransportOptions {
    pub(crate) init_stream_window_size: Option<u32>,
    pub(crate) init_connection_window_size: Option<u32>,
    pub(crate) http2_keep_alive_interval: Option<Duration>,
    pub(crate) http2_keep_alive_timeout: Option<Duration>,
    pub(crate) http2_keep_alive_while_idle: Option<bool>,
    pub(crate) http2_max_header_list_size: Option<u32>,
    pub(crate) http2_adaptive_window: Option<bool>,
    pub(crate) concurrency_limit: Option<usize>,
    pub(crate) rate_limit: Option<(u64, Duration)>,
    pub(crate) tcp_keepalive: Option<Duration>,
    pub(crate) tcp_nodelay: bool,
    pub(crate) connect_deadline: Option<Instant>,
}

#[async_trait]
pub(crate) trait Transport: Send + Sync {
    async fn connect(
        &self,
        address: String,
        runtime: Arc<dyn Runtime>,
        opts: &TransportOptions,
    ) -> Result<ConnectedTransport, String>;
}
