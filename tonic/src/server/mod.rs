//! Generic server implementation.
//!
//! This module contains the low level components to build a gRPC server. It
//! provides a codec agnostic gRPC server handler.
//!
//! The items in this module are generally designed to be used by some codegen
//! tool that will provide the user some custom way to implement the server that
//! will implement the proper gRPC service. Thusly, they are a bit hard to use
//! by hand.

mod grpc;
mod service;

pub use self::grpc::Grpc;
pub use self::service::{
    ClientStreamingService, ServerStreamingService, StreamingService, UnaryService,
};
