//! A `tonic` based gRPC Metrics implementation.

use std::time::Instant;

use tonic::reporter::{ReporterCallback, RpcType};
use tonic::{Code, Status};

#[derive(Clone)]
struct MetricsReporterCallback {
    service_name: &'static str,
    method_name: &'static str,
    rpc_type: &'static str,
    start_time: Instant,
}

/// Returns a reporting callback that implement metrics compatible with
/// https://github.com/grpc-ecosystem/go-grpc-prometheus.
pub fn metrics_reporter_fn(
    service_name: &'static str,
    method_name: &'static str,
    rpc_type: RpcType,
) -> Box<dyn ReporterCallback + Send + Sync + 'static> {
    metrics::increment_counter!(
        "grpc_server_started_total",
        "grpc_type" => convert_rpc_type(rpc_type),
        "grpc_service" => service_name,
        "grpc_method" => method_name,
    );

    let callback = MetricsReporterCallback {
        service_name,
        method_name,
        rpc_type: convert_rpc_type(rpc_type),
        start_time: Instant::now(),
    };
    Box::new(callback)
}

impl ReporterCallback for MetricsReporterCallback {
    fn rpc_complete(&self, status: Status) {
        let elapsed = self.start_time.elapsed();

        metrics::increment_counter!(
            "grpc_server_handled_total",
            "grpc_type" => self.rpc_type,
            "grpc_service" => self.service_name,
            "grpc_method" => self.method_name,
            "grpc_code" => convert_status_code(status.code()),
        );

        metrics::histogram!(
            "grpc_server_handling_seconds",
            elapsed,
            "grpc_type" => self.rpc_type,
            "grpc_service" => self.service_name,
            "grpc_method" => self.method_name,
        );
    }

    fn stream_message_received(&self) {
        metrics::increment_counter!(
            "grpc_server_msg_received_total",
            "grpc_type" => self.rpc_type,
            "grpc_service" => self.service_name,
            "grpc_method" => self.method_name,
        );
    }

    fn stream_message_sent(&self) {
        metrics::increment_counter!(
            "grpc_server_msg_sent_total",
            "grpc_type" => self.rpc_type,
            "grpc_service" => self.service_name,
            "grpc_method" => self.method_name,
        );
    }
}

#[inline]
fn convert_status_code(code: Code) -> &'static str {
    match code {
        Code::Ok => "OK",
        Code::Cancelled => "Canceled",
        Code::Unknown => "Unknown",
        Code::InvalidArgument => "InvalidArgument",
        Code::DeadlineExceeded => "DeadlineExceeded",
        Code::NotFound => "NotFound",
        Code::AlreadyExists => "AlreadyExists",
        Code::PermissionDenied => "PermissionDenied",
        Code::ResourceExhausted => "ResourceExhausted",
        Code::FailedPrecondition => "FailedPrecondition",
        Code::Aborted => "Aborted",
        Code::OutOfRange => "OutOfRange",
        Code::Unimplemented => "Unimplemented",
        Code::Internal => "Internal",
        Code::Unavailable => "Unavailable",
        Code::DataLoss => "DataLoss",
        Code::Unauthenticated => "Unauthenticated",
        _ => "**INVALID**",
    }
}

#[inline]
fn convert_rpc_type(rpc_type: RpcType) -> &'static str {
    match rpc_type {
        RpcType::Unary => "unary",
        RpcType::ClientStreaming => "client_stream",
        RpcType::ServerStreaming => "server_stream",
        RpcType::Streaming => "bidi_stream",
    }
}
