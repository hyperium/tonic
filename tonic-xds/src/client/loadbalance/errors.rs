//! Errors for the load balancer.

/// Errors produced by the load balancer.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LbError {
    /// No ready endpoints available to serve the request.
    #[error("no ready endpoints available")]
    Unavailable,

    /// The selected lb channel was not ready.
    #[error("lb channel not ready: {0}")]
    LbChannelPollReadyError(tower::BoxError),

    /// The selected lb channel returned an error.
    #[error("lb channel error: {0}")]
    LbChannelCallError(tower::BoxError),

    /// Discovery stream produced an error.
    #[error("discovery error: {0}")]
    DiscoverError(tower::BoxError),

    /// Discovery stream is closed (returned None).
    #[error("discovery stream is closed")]
    DiscoverClosed,

    /// Internal precondition violation (bug).
    #[error("failed precondition: {0}")]
    FailedPrecondition(&'static str),

    /// Discovery is closed and no endpoints are connecting or ready —
    /// no progress is possible, fail fast instead of hanging.
    #[error("load balancer is stagnant: discovery is closed and no endpoints are available")]
    Stagnation,
}

impl From<LbError> for tonic::Status {
    fn from(err: LbError) -> Self {
        match err {
            LbError::Unavailable => tonic::Status::unavailable("no ready endpoints available"),
            LbError::LbChannelPollReadyError(inner) => tonic::Status::unavailable(format!(
                "error when polling readiness of lb channel: {inner}"
            )),
            LbError::DiscoverError(source) => {
                tonic::Status::unavailable(format!("discovery error: {source}"))
            }
            LbError::DiscoverClosed => tonic::Status::unavailable("discovery stream is closed"),
            LbError::FailedPrecondition(msg) => tonic::Status::failed_precondition(msg),
            LbError::Stagnation => tonic::Status::unavailable(
                "load balancer is stagnant: discovery is closed and no endpoints are available",
            ),
            LbError::LbChannelCallError(source) => match source.downcast::<tonic::Status>() {
                Ok(status) => *status,
                Err(source) => tonic::Status::unknown(format!("lb channel error: {source}")),
            },
        }
    }
}
