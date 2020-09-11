mod call;
mod cors;

mod service;
pub use service::GrpcWeb;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
