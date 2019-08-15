use crate::{Request, Response, Status};
use futures_core::Stream;
use std::future::Future;

pub trait UnaryService<R> {
    /// Protobuf response message type
    type Response;

    /// Response future
    type Future: Future<Output = Result<Response<Self::Response>, Status>>;

    /// Call the service
    fn call(&mut self, request: Request<R>) -> Self::Future;
}

pub trait ServerStreamingService<R> {
    /// Protobuf response message type
    type Response;

    /// Stream of outbound response messages
    type ResponseStream: Stream<Item = Result<Self::Response, Status>> + Unpin;

    /// Response future
    type Future: Future<Output = Result<Response<Self::ResponseStream>, Status>>;

    /// Call the service
    fn call(&mut self, request: Request<R>) -> Self::Future;
}

pub trait ClientStreamingService<RequestStream> {
    /// Protobuf response message type
    type Response;

    /// Response future
    type Future: Future<Output = Result<Response<Self::Response>, Status>>;

    /// Call the service
    fn call(&mut self, request: Request<RequestStream>) -> Self::Future;
}

pub trait StreamingService<RequestStream> {
    /// Protobuf response message type
    type Response;

    /// Stream of outbound response messages
    type ResponseStream: Stream<Item = Result<Self::Response, Status>> + Unpin;

    /// Response future
    type Future: Future<Output = Result<Response<Self::ResponseStream>, Status>>;

    /// Call the service
    fn call(&mut self, request: Request<RequestStream>) -> Self::Future;
}
