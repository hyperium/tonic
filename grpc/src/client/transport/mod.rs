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

use std::time::Duration;
use std::time::Instant;

use crate::client::subchannel::SharedServiceTrait;
use crate::rt::GrpcRuntime;

mod registry;

// Using tower/buffer enables tokio's rt feature even though it's possible to
// create Buffers with a user provided executor.
#[cfg(feature = "_runtime-tokio")]
mod tonic;

use ::tonic::async_trait;
pub(crate) use registry::GLOBAL_TRANSPORT_REGISTRY;
pub(crate) use registry::TransportRegistry;
use tokio::sync::oneshot;

pub(crate) struct ConnectedTransport {
    pub service: Box<dyn SharedServiceTrait>,
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
        runtime: GrpcRuntime,
        opts: &TransportOptions,
    ) -> Result<ConnectedTransport, String>;
}
