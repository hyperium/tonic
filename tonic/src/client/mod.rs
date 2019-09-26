//! Generic client implementation.
//!
//! This module contains the low level components to build a gRPC client. It
//! provides a codec agnostic gRPC client dispatcher and a decorated tower
//! service trait.
//!
//! This client is generally used by some code generation tool to provide stubs
//! for the gRPC service. Thusly, they are a bit cumbersome to use by hand.

mod grpc;
mod service;
mod message;

pub use self::grpc::Grpc;
pub use self::message::{IntoRequest, Message};
pub use self::service::GrpcService;
