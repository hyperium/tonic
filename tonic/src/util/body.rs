use bytes::{Buf, Bytes};
use http_body::Body;

use crate::{
    body::{empty_body, local_empty_body},
    transport::{LocalExec, TokioExec},
    Status,
};

pub trait HasEmptyBody {
    type BoxBody: Body<Data = Bytes, Error = crate::Status> + Unpin;

    fn empty_body() -> Self::BoxBody;
}

impl HasEmptyBody for TokioExec {
    type BoxBody = crate::body::BoxBody;

    fn empty_body() -> Self::BoxBody {
        empty_body()
    }
}

impl HasEmptyBody for LocalExec {
    type BoxBody = crate::body::LocalBoxBody;

    fn empty_body() -> Self::BoxBody {
        local_empty_body()
    }
}

pub trait MaybeSend<B>: HasEmptyBody {}

impl<B> MaybeSend<B> for TokioExec where B: Send {}

impl<B> MaybeSend<B> for LocalExec {}

pub trait HasBoxedBody<B>: MaybeSend<B> {
    fn boxed(body: B) -> Self::BoxBody;
}

impl<B> HasBoxedBody<B> for TokioExec
where
    B: Body<Data = Bytes, Error = Status> + Send + 'static,
    B::Error: Into<crate::Error>,
{
    fn boxed(body: B) -> Self::BoxBody {
        Self::BoxBody::new(body)
    }
}

impl<B> HasBoxedBody<B> for LocalExec
where
    B: Body<Data = Bytes, Error = Status> + 'static,
    B::Error: Into<crate::Error>,
{
    fn boxed(body: B) -> Self::BoxBody {
        Self::BoxBody::new(body)
    }
}

pub trait HasBoxedBodyWithMapErr<B>: MaybeSend<B> {
    fn boxed_with_map_err(body: B) -> Self::BoxBody;
}

impl<B> HasBoxedBodyWithMapErr<B> for TokioExec
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<crate::Error>,
{
    fn boxed_with_map_err(body: B) -> Self::BoxBody {
        Self::BoxBody::new(body.map_err(crate::Status::map_error))
    }
}

impl<B> HasBoxedBodyWithMapErr<B> for LocalExec
where
    B: Body<Data = Bytes> + 'static,
    B::Error: Into<crate::Error>,
{
    fn boxed_with_map_err(body: B) -> Self::BoxBody {
        Self::BoxBody::new(body.map_err(crate::Status::map_error))
    }
}

pub trait HasBoxedBodyWithMapDataErr<B>: MaybeSend<B> {
    fn boxed_with_map_data_err(body: B) -> Self::BoxBody;
}

impl<B> HasBoxedBodyWithMapDataErr<B> for TokioExec
where
    B: Body + Send + 'static,
    B::Error: Into<crate::Error>,
{
    fn boxed_with_map_data_err(body: B) -> Self::BoxBody {
        Self::BoxBody::new(
            body.map_data(|mut buf| buf.copy_to_bytes(buf.remaining()))
                .map_err(Status::map_error),
        )
    }
}

impl<B> HasBoxedBodyWithMapDataErr<B> for LocalExec
where
    B: Body + 'static,
    B::Error: Into<crate::Error>,
{
    fn boxed_with_map_data_err(body: B) -> Self::BoxBody {
        Self::BoxBody::new(
            body.map_data(|mut buf| buf.copy_to_bytes(buf.remaining()))
                .map_err(Status::map_error),
        )
    }
}
