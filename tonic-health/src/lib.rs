use std::fmt::{Display, Formatter};

mod proto {
    tonic::include_proto!("grpc.health.v1");
}

pub mod server;

/// An enumeration of values representing gRPC service health.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ServingStatus {
    Unknown,
    Serving,
    NotServing,
}

impl Display for ServingStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ServingStatus::Unknown => f.write_str("Unknown"),
            ServingStatus::Serving => f.write_str("Serving"),
            ServingStatus::NotServing => f.write_str("NotServing"),
        }
    }
}

impl From<ServingStatus> for proto::health_check_response::ServingStatus {
    fn from(s: ServingStatus) -> Self {
        match s {
            ServingStatus::Unknown => proto::health_check_response::ServingStatus::Unknown,
            ServingStatus::Serving => proto::health_check_response::ServingStatus::Serving,
            ServingStatus::NotServing => proto::health_check_response::ServingStatus::NotServing,
        }
    }
}
