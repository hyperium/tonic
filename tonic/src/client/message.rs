#![allow(missing_docs)]

use crate::Request;

pub trait Message: std::fmt::Debug {}

impl<T: prost::Message> Message for T {}

pub trait IntoRequest {
    type Message: Message;

    fn into_request(self) -> Request<Self::Message>;
}

impl<T: Message> IntoRequest for T {
    type Message = T;

    fn into_request(self) -> Request<T> {
        Request::new(self)
    }
}

impl<T: Message> IntoRequest for Request<T> {
    type Message = T;

    fn into_request(self) -> Request<T> {
        self
    }
}
