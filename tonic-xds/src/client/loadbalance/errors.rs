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
    Discover(tower::BoxError),

    /// Internal precondition violation (bug).
    #[error("failed precondition: {0}")]
    FailedPrecondition(&'static str),
}

impl From<LbError> for tonic::Status {
    fn from(err: LbError) -> Self {
        match err {
            LbError::Unavailable => tonic::Status::unavailable("no ready endpoints available"),
            LbError::LbChannelPollReadyError(inner) => tonic::Status::unavailable(format!(
                "error when polling readiness of lb channel: {inner}"
            )),
            LbError::Discover(source) => {
                tonic::Status::unavailable(format!("discovery error: {source}"))
            }
            LbError::FailedPrecondition(msg) => tonic::Status::failed_precondition(msg),
            LbError::LbChannelCallError(source) => match source.downcast::<tonic::Status>() {
                Ok(status) => *status,
                Err(source) => tonic::Status::unknown(format!("lb channel error: {source}")),
            },
        }
    }
}
