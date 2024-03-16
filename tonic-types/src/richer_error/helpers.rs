use prost::{
    bytes::{Bytes, BytesMut},
    DecodeError, Message,
};
use prost_types::Any;
use tonic::Code;

use crate::pb;

pub(super) trait IntoAny {
    fn into_any(self) -> Any;
}

pub(super) trait FromAnyRef {
    fn from_any_ref(any: &Any) -> Result<Self, DecodeError>
    where
        Self: Sized;
}

pub(super) fn gen_details_bytes(code: Code, message: &str, details: Vec<Any>) -> Bytes {
    let status = pb::Status {
        code: code as i32,
        message: message.to_owned(),
        details,
    };

    let mut buf = BytesMut::with_capacity(status.encoded_len());

    // Should never panic since `buf` is initialized with sufficient capacity
    status.encode(&mut buf).unwrap();

    buf.freeze()
}
