//! Contains data structures and utilities for reporting of RPC life cycle events.

use std::fmt;
use std::sync::Arc;

use crate::Status;

/// The type of RPC call
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RpcType {
    /// Unary RPC call
    Unary,

    /// Client streaming RPC call
    ClientStreaming,

    /// Server streaming RPC call
    ServerStreaming,

    /// Bidirectional streaming RPC call
    Streaming,
}

/// Type alias for functions used to receive the initial report callback.
type ReporterFn = Arc<
    dyn Fn(&'static str, &'static str, RpcType) -> Box<dyn ReporterCallback + Send + Sync + 'static>
        + Send
        + Sync
        + 'static,
>;

/// Represents a gRPC reporter.
///
/// A gRPC reporter provides a way to receive notifications during the life cycle of RPC calls.
/// The main intended use for `Reporter` is for generating time-series metrics (and possibily
/// tracing spans related to RPC calls in the future).
///
/// See the `tonic-metrics` crate for an example of a reporting callback that generates
/// time-series metrics similar to what the Go GRPC client has available in an add-on module.
#[derive(Clone)]
pub struct Reporter {
    f: ReporterFn,
}

impl Reporter {
    /// Create a new `Reporter` from the given callback closure.
    ///
    /// The callback function will receive the fully-qualified service name, method name, and
    /// RPC type. It returns a boxed instance of ReporterCallback which receives future
    /// reporting events relating to the same RPC.
    pub fn new(
        f: impl Fn(
                &'static str,
                &'static str,
                RpcType,
            ) -> Box<dyn ReporterCallback + Send + Sync + 'static>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Reporter { f: Arc::new(f) }
    }

    /// Helper function for invoking the callback closure.
    pub(crate) fn report_rpc_start(
        &self,
        service_path: &'static str,
        method_name: &'static str,
        rpc_type: RpcType,
    ) -> Box<dyn ReporterCallback + Send + Sync + 'static> {
        (self.f)(service_path, method_name, rpc_type)
    }
}

impl<F> From<F> for Reporter
where
    F: Fn(&'static str, &'static str, RpcType) -> Box<dyn ReporterCallback + Send + Sync + 'static>
        + Send
        + Sync
        + 'static,
{
    fn from(f: F) -> Self {
        Reporter::new(f)
    }
}

/// Reporting callbacks for a single RPC
pub trait ReporterCallback {
    /// Called when the RPC completes regardless of success or failure.
    fn rpc_complete(&self, status: Status);

    /// Called each time a stream message is received from the remote peer.
    fn stream_message_received(&self);

    /// Called each time a stream message is sent to the remote peer.
    fn stream_message_sent(&self);
}

impl<R> ReporterCallback for Box<R>
where
    R: ReporterCallback + Send + Sync + 'static + ?Sized,
{
    fn rpc_complete(&self, status: Status) {
        (**self).rpc_complete(status)
    }

    fn stream_message_received(&self) {
        (**self).stream_message_received();
    }

    fn stream_message_sent(&self) {
        (**self).stream_message_sent();
    }
}

impl<R> ReporterCallback for Arc<R>
where
    R: ReporterCallback + Send + Sync + 'static + ?Sized,
{
    fn rpc_complete(&self, status: Status) {
        (**self).rpc_complete(status)
    }

    fn stream_message_received(&self) {
        (**self).stream_message_received();
    }

    fn stream_message_sent(&self) {
        (**self).stream_message_sent();
    }
}

impl fmt::Debug for Reporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reporter").finish()
    }
}
