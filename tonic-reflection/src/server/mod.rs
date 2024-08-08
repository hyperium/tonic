pub use crate::pb::v1::server_reflection_server::{ServerReflection, ServerReflectionServer};

use prost::DecodeError;
use std::fmt::{Display, Formatter};

mod parser;

mod v1;

pub use v1::Builder;

/// Represents an error in the construction of a gRPC Reflection Service.
#[derive(Debug)]
pub enum Error {
    /// An error was encountered decoding a `prost_types::FileDescriptorSet` from a buffer.
    DecodeError(prost::DecodeError),
    /// An invalid `prost_types::FileDescriptorProto` was encountered.
    InvalidFileDescriptorSet(String),
}

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Error::DecodeError(e)
    }
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::DecodeError(_) => f.write_str("error decoding FileDescriptorSet from buffer"),
            Error::InvalidFileDescriptorSet(s) => {
                write!(f, "invalid FileDescriptorSet - {}", s)
            }
        }
    }
}
