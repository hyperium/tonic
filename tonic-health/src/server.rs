//! Contains all healthcheck based server utilities.

use crate::pb::health_server::{Health, HealthServer};
use crate::pb::{HealthCheckRequest, HealthCheckResponse};
use crate::ServingStatus;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{watch, RwLock};
use tokio_stream::Stream;
#[cfg(feature = "transport")]
use tonic::server::NamedService;
use tonic::{Request, Response, Status};

/// Creates a `HealthReporter` and a linked `HealthServer` pair. Together,
/// these types can be used to serve the gRPC Health Checking service.
///
/// A `HealthReporter` is used to update the state of gRPC services.
///
/// A `HealthServer` is a Tonic gRPC server for the `grpc.health.v1.Health`,
/// which can be added to a Tonic runtime using `add_service` on the runtime
/// builder.
pub fn health_reporter() -> (HealthReporter, HealthServer<impl Health>) {
    let reporter = HealthReporter::new();
    let service = HealthService::new(reporter.statuses.clone());
    let server = HealthServer::new(service);

    (reporter, server)
}

type StatusPair = (watch::Sender<ServingStatus>, watch::Receiver<ServingStatus>);

/// A handle providing methods to update the health status of gRPC services. A
/// `HealthReporter` is connected to a `HealthServer` which serves the statuses
/// over the `grpc.health.v1.Health` service.
#[derive(Clone, Debug)]
pub struct HealthReporter {
    statuses: Arc<RwLock<HashMap<String, StatusPair>>>,
}

impl HealthReporter {
    fn new() -> Self {
        // According to the gRPC Health Check specification, the empty service "" corresponds to the overall server health
        let server_status = ("".to_string(), watch::channel(ServingStatus::Serving));

        let statuses = Arc::new(RwLock::new(HashMap::from([server_status])));

        HealthReporter { statuses }
    }

    /// Sets the status of the service implemented by `S` to `Serving`. This notifies any watchers
    /// if there is a change in status.
    #[cfg(feature = "transport")]
    #[cfg_attr(docsrs, doc(cfg(feature = "transport")))]
    pub async fn set_serving<S>(&mut self)
    where
        S: NamedService,
    {
        let service_name = <S as NamedService>::NAME;
        self.set_service_status(service_name, ServingStatus::Serving)
            .await;
    }

    /// Sets the status of the service implemented by `S` to `NotServing`. This notifies any watchers
    /// if there is a change in status.
    #[cfg(feature = "transport")]
    #[cfg_attr(docsrs, doc(cfg(feature = "transport")))]
    pub async fn set_not_serving<S>(&mut self)
    where
        S: NamedService,
    {
        let service_name = <S as NamedService>::NAME;
        self.set_service_status(service_name, ServingStatus::NotServing)
            .await;
    }

    /// Sets the status of the service with `service_name` to `status`. This notifies any watchers
    /// if there is a change in status.
    pub async fn set_service_status<S>(&mut self, service_name: S, status: ServingStatus)
    where
        S: AsRef<str>,
    {
        let service_name = service_name.as_ref();
        let mut writer = self.statuses.write().await;
        match writer.get(service_name) {
            Some((tx, _)) => {
                // We only ever hand out clones of the receiver, so the originally-created
                // receiver should always be present, only being dropped when clearing the
                // service status. Consequently, `tx.send` should not fail, making use
                // of `expect` here safe.
                tx.send(status).expect("channel should not be closed");
            }
            None => {
                writer.insert(service_name.to_string(), watch::channel(status));
            }
        };
    }

    /// Clear the status of the given service.
    pub async fn clear_service_status(&mut self, service_name: &str) {
        let mut writer = self.statuses.write().await;
        let _ = writer.remove(service_name);
    }
}

/// A service providing implementations of gRPC health checking protocol.
#[derive(Debug)]
pub struct HealthService {
    statuses: Arc<RwLock<HashMap<String, StatusPair>>>,
}

impl HealthService {
    fn new(services: Arc<RwLock<HashMap<String, StatusPair>>>) -> Self {
        HealthService { statuses: services }
    }

    async fn service_health(&self, service_name: &str) -> Option<ServingStatus> {
        let reader = self.statuses.read().await;
        reader.get(service_name).map(|p| *p.1.borrow())
    }
}

#[tonic::async_trait]
impl Health for HealthService {
    async fn check(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        let service_name = request.get_ref().service.as_str();
        let status = self.service_health(service_name).await;

        match status {
            None => Err(Status::not_found("service not registered")),
            Some(status) => Ok(Response::new(HealthCheckResponse {
                status: crate::pb::health_check_response::ServingStatus::from(status) as i32,
            })),
        }
    }

    type WatchStream =
        Pin<Box<dyn Stream<Item = Result<HealthCheckResponse, Status>> + Send + 'static>>;

    async fn watch(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let service_name = request.get_ref().service.as_str();
        let mut status_rx = match self.statuses.read().await.get(service_name) {
            None => return Err(Status::not_found("service not registered")),
            Some(pair) => pair.1.clone(),
        };

        let output = async_stream::try_stream! {
            // yield the current value
            let status = crate::pb::health_check_response::ServingStatus::from(*status_rx.borrow()) as i32;
            yield HealthCheckResponse { status };

            #[allow(clippy::redundant_pattern_matching)]
            while let Ok(_) = status_rx.changed().await {
                let status = crate::pb::health_check_response::ServingStatus::from(*status_rx.borrow()) as i32;
                yield HealthCheckResponse { status };
            }
        };

        Ok(Response::new(Box::pin(output) as Self::WatchStream))
    }
}

#[cfg(test)]
mod tests {
    use crate::pb::health_server::Health;
    use crate::pb::HealthCheckRequest;
    use crate::server::{HealthReporter, HealthService};
    use crate::ServingStatus;
    use tokio::sync::watch;
    use tokio_stream::StreamExt;
    use tonic::{Code, Request, Status};

    fn assert_serving_status(wire: i32, expected: ServingStatus) {
        let expected = crate::pb::health_check_response::ServingStatus::from(expected) as i32;
        assert_eq!(wire, expected);
    }

    fn assert_grpc_status(wire: Option<Status>, expected: Code) {
        let wire = wire.expect("status is not None").code();
        assert_eq!(wire, expected);
    }

    async fn make_test_service() -> (HealthReporter, HealthService) {
        let health_reporter = HealthReporter::new();

        // insert test value
        {
            let mut statuses = health_reporter.statuses.write().await;
            statuses.insert(
                "TestService".to_string(),
                watch::channel(ServingStatus::Unknown),
            );
        }

        let health_service = HealthService::new(health_reporter.statuses.clone());
        (health_reporter, health_service)
    }

    #[tokio::test]
    async fn test_service_check() {
        let (mut reporter, service) = make_test_service().await;

        // Overall server health
        let resp = service
            .check(Request::new(HealthCheckRequest {
                service: "".to_string(),
            }))
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap().into_inner();
        assert_serving_status(resp.status, ServingStatus::Serving);

        // Unregistered service
        let resp = service
            .check(Request::new(HealthCheckRequest {
                service: "Unregistered".to_string(),
            }))
            .await;
        assert!(resp.is_err());
        assert_grpc_status(resp.err(), Code::NotFound);

        // Registered service - initial state
        let resp = service
            .check(Request::new(HealthCheckRequest {
                service: "TestService".to_string(),
            }))
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap().into_inner();
        assert_serving_status(resp.status, ServingStatus::Unknown);

        // Registered service - updated state
        reporter
            .set_service_status("TestService", ServingStatus::Serving)
            .await;
        let resp = service
            .check(Request::new(HealthCheckRequest {
                service: "TestService".to_string(),
            }))
            .await;
        assert!(resp.is_ok());
        let resp = resp.unwrap().into_inner();
        assert_serving_status(resp.status, ServingStatus::Serving);
    }

    #[tokio::test]
    async fn test_service_watch() {
        let (mut reporter, service) = make_test_service().await;

        // Overall server health
        let resp = service
            .watch(Request::new(HealthCheckRequest {
                service: "".to_string(),
            }))
            .await;
        assert!(resp.is_ok());
        let mut resp = resp.unwrap().into_inner();
        let item = resp
            .next()
            .await
            .expect("streamed response is Some")
            .expect("response is ok");
        assert_serving_status(item.status, ServingStatus::Serving);

        // Unregistered service
        let resp = service
            .watch(Request::new(HealthCheckRequest {
                service: "Unregistered".to_string(),
            }))
            .await;
        assert!(resp.is_err());
        assert_grpc_status(resp.err(), Code::NotFound);

        // Registered service
        let resp = service
            .watch(Request::new(HealthCheckRequest {
                service: "TestService".to_string(),
            }))
            .await;
        assert!(resp.is_ok());
        let mut resp = resp.unwrap().into_inner();

        // Registered service - initial state
        let item = resp
            .next()
            .await
            .expect("streamed response is Some")
            .expect("response is ok");
        assert_serving_status(item.status, ServingStatus::Unknown);

        // Registered service - updated state
        reporter
            .set_service_status("TestService", ServingStatus::NotServing)
            .await;

        let item = resp
            .next()
            .await
            .expect("streamed response is Some")
            .expect("response is ok");
        assert_serving_status(item.status, ServingStatus::NotServing);

        // Registered service - updated state
        reporter
            .set_service_status("TestService", ServingStatus::Serving)
            .await;
        let item = resp
            .next()
            .await
            .expect("streamed response is Some")
            .expect("response is ok");
        assert_serving_status(item.status, ServingStatus::Serving);

        // De-registered service
        reporter.clear_service_status("TestService").await;
        let item = resp.next().await;
        assert!(item.is_none());
    }
}
