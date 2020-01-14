use crate::{Request, Status};
use std::{fmt, sync::Arc};

/// Represents a gRPC interceptor.
///
/// gRPC interceptors are similar to middleware but have much less
/// flexibility. This interceptor allows you to do two main things,
/// one is to add/remove/check items in the `MetadataMap` of each
/// request. Two, cancel a request with any `Status`.
///
/// An interceptor can be used on both the server and client side through
/// the `tonic-build` crate's generated structs.
///
/// These interceptors do not allow you to modify the `Message` of the request
/// but allow you to check for metadata. If you would like to apply middleware like
/// features to the body of the request, going through the `tower` abstraction is recommended.
#[derive(Clone)]
pub struct Interceptor {
    f: Arc<dyn Fn(Request<()>) -> Result<Request<()>, Status> + Send + Sync + 'static>,
}

impl Interceptor {
    /// Create a new `Interceptor` from the provided function.
    pub fn new(
        f: impl Fn(Request<()>) -> Result<Request<()>, Status> + Send + Sync + 'static,
    ) -> Self {
        Interceptor { f: Arc::new(f) }
    }

    pub(crate) fn call<T>(&self, req: Request<T>) -> Result<Request<T>, Status> {
        let (metadata, ext, message) = req.into_parts();

        let temp_req = Request::from_parts(metadata, ext, ());

        let (metadata, ext, _) = (self.f)(temp_req)?.into_parts();

        Ok(Request::from_parts(metadata, ext, message))
    }
}

impl<F> From<F> for Interceptor
where
    F: Fn(Request<()>) -> Result<Request<()>, Status> + Send + Sync + 'static,
{
    fn from(f: F) -> Self {
        Interceptor::new(f)
    }
}

impl fmt::Debug for Interceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interceptor").finish()
    }
}
