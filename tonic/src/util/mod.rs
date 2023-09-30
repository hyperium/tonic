//! Various utilities used throughout tonic.

// some combinations of features might cause things here not to be used
#![allow(dead_code)]

pub(crate) mod body;

use std::{
    convert::Infallible,
    future::Future,
    task::{Context, Poll},
};

use http::{Request, Response};
use hyper::Body;
use tower::ServiceExt;
use tower_layer::{layer_fn, LayerFn};
use tower_service::Service;

use crate::{
    body::{BoxBody, LocalBoxBody},
    codegen::{BoxFuture, LocalBoxFuture},
};

pub(crate) mod base64 {
    use base64::{
        alphabet,
        engine::{
            general_purpose::{GeneralPurpose, GeneralPurposeConfig},
            DecodePaddingMode,
        },
    };

    pub(crate) const STANDARD: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_encode_padding(true)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );

    pub(crate) const STANDARD_NO_PAD: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
}

/// A trait unifying `BoxCloneService` and `LocalBoxCloneService`.
pub trait BoxCloneService:
    Service<Request<Body>, Response = Response<Self::BoxBody>, Future = Self::BoxFuture> + Clone
{
    type BoxBody;
    type BoxFuture: Future<Output = Result<Response<Self::BoxBody>, Infallible>>;

    fn empty_response() -> Self::BoxFuture;
}

impl BoxCloneService
    for tower::util::BoxCloneService<Request<Body>, Response<BoxBody>, Infallible>
{
    type BoxBody = BoxBody;
    type BoxFuture = BoxFuture<Response<Self::BoxBody>, Infallible>;

    fn empty_response() -> Self::BoxFuture {
        Box::pin(async move {
            Ok(Response::builder()
                .status(http::StatusCode::OK)
                .header("grpc-status", "12")
                .header("content-type", "application/grpc")
                .body(crate::body::empty_body())
                .unwrap())
        })
    }
}

impl BoxCloneService
    for crate::util::LocalBoxCloneService<Request<Body>, Response<LocalBoxBody>, Infallible>
{
    type BoxBody = LocalBoxBody;
    type BoxFuture = LocalBoxFuture<Response<Self::BoxBody>, Infallible>;

    fn empty_response() -> Self::BoxFuture {
        Box::pin(async move {
            Ok(Response::builder()
                .status(http::StatusCode::OK)
                .header("grpc-status", "12")
                .header("content-type", "application/grpc")
                .body(crate::body::local_empty_body())
                .unwrap())
        })
    }
}

#[allow(missing_docs, missing_debug_implementations)]
pub struct LocalBoxCloneService<T, U, E>(
    Box<dyn CloneService<T, Response = U, Error = E, Future = LocalBoxFuture<U, E>>>,
);

impl<T, U, E> LocalBoxCloneService<T, U, E> {
    /// Create a new `BoxCloneService`.
    pub(crate) fn new<S>(inner: S) -> Self
    where
        S: Service<T, Response = U, Error = E> + Clone + 'static,
        S::Future: 'static,
    {
        let inner = inner.map_future(|f| Box::pin(f) as _);
        Self(Box::new(inner))
    }

    /// Returns a [`Layer`] for wrapping a [`Service`] in a [`BoxCloneService`]
    /// middleware.
    ///
    /// [`Layer`]: crate::Layer
    pub(crate) fn layer<S>() -> LayerFn<fn(S) -> Self>
    where
        S: Service<T, Response = U, Error = E> + Clone + 'static,
        S::Future: 'static,
    {
        layer_fn(Self::new)
    }
}

impl<T, U, E> Service<T> for LocalBoxCloneService<T, U, E> {
    type Response = U;
    type Error = E;
    type Future = LocalBoxFuture<U, E>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), E>> {
        self.0.poll_ready(cx)
    }

    #[inline]
    fn call(&mut self, request: T) -> Self::Future {
        self.0.call(request)
    }
}

impl<T, U, E> Clone for LocalBoxCloneService<T, U, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

trait CloneService<R>: Service<R> {
    fn clone_box(
        &self,
    ) -> Box<
        dyn CloneService<R, Response = Self::Response, Error = Self::Error, Future = Self::Future>,
    >;
}

impl<R, T> CloneService<R> for T
where
    T: Service<R> + Clone + 'static,
{
    fn clone_box(
        &self,
    ) -> Box<dyn CloneService<R, Response = T::Response, Error = T::Error, Future = T::Future>>
    {
        Box::new(self.clone())
    }
}
