//! Example: minimal xDS control plane for testing.
//!
//! Serves a static set of LDS/CDS/EDS resources via the ADS
//! (Aggregated Discovery Service) SotW protocol. Useful for testing
//! the `channel` example without an external control plane.
//!
//! # Quick start
//!
//! ```sh
//! ./tonic-xds/examples/run_xds_example.sh
//! ```
//!
//! # Running individually
//!
//! ```sh
//! # Defaults: listener "my-service", endpoint 127.0.0.1:50051
//! cargo run -p tonic-xds --example xds_server
//!
//! # Custom endpoints:
//! ENDPOINTS=127.0.0.1:50051,127.0.0.1:50052 cargo run -p tonic-xds --example xds_server
//! ```
//!
//! # Configuration
//!
//! - `LISTENER_NAME` — listener name to serve (default: `my-service`)
//! - `CLUSTER_NAME` — cluster name (default: `my-cluster`)
//! - `ENDPOINTS` — comma-separated `host:port` list (default: `127.0.0.1:50051`)
//! - `PORT` — server listen port (default: `18000`)

use envoy_types::pb::envoy::config::cluster::v3::Cluster;
use envoy_types::pb::envoy::config::cluster::v3::cluster::DiscoveryType;
use envoy_types::pb::envoy::config::core::v3 as core_v3;
use envoy_types::pb::envoy::config::endpoint::v3::{
    ClusterLoadAssignment, LbEndpoint, LocalityLbEndpoints, lb_endpoint::HostIdentifier,
};
use envoy_types::pb::envoy::config::listener::v3::{ApiListener, Listener};
use envoy_types::pb::envoy::config::route::v3::route::Action;
use envoy_types::pb::envoy::config::route::v3::route_action::ClusterSpecifier;
use envoy_types::pb::envoy::config::route::v3::route_match::PathSpecifier;
use envoy_types::pb::envoy::config::route::v3::{
    Route, RouteAction, RouteConfiguration, RouteMatch, VirtualHost,
};
use envoy_types::pb::envoy::extensions::filters::network::http_connection_manager::v3::{
    HttpConnectionManager, http_connection_manager::RouteSpecifier,
};
use envoy_types::pb::envoy::service::discovery::v3::{
    DeltaDiscoveryRequest, DeltaDiscoveryResponse, DiscoveryRequest, DiscoveryResponse,
    aggregated_discovery_service_server::{
        AggregatedDiscoveryService, AggregatedDiscoveryServiceServer,
    },
};
use envoy_types::pb::google::protobuf::Any;
use prost::Message;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status};

const TYPE_LISTENER: &str = "type.googleapis.com/envoy.config.listener.v3.Listener";

const TYPE_CLUSTER: &str = "type.googleapis.com/envoy.config.cluster.v3.Cluster";
const TYPE_ENDPOINTS: &str = "type.googleapis.com/envoy.config.endpoint.v3.ClusterLoadAssignment";
const TYPE_HCM: &str = "type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager";

/// Static xDS resource snapshot, keyed by type URL.
struct Snapshot {
    resources: HashMap<String, Vec<Any>>,
}

impl Snapshot {
    fn build(listener_name: &str, cluster_name: &str, endpoints: &[(String, u32)]) -> Self {
        let mut resources: HashMap<String, Vec<Any>> = HashMap::new();
        let route_config_name = format!("{listener_name}-route");

        // LDS: Listener → HttpConnectionManager (inline route config)
        let hcm = HttpConnectionManager {
            route_specifier: Some(RouteSpecifier::RouteConfig(RouteConfiguration {
                name: route_config_name.clone(),
                virtual_hosts: vec![VirtualHost {
                    name: "default".to_string(),
                    domains: vec!["*".to_string()],
                    routes: vec![Route {
                        r#match: Some(RouteMatch {
                            path_specifier: Some(PathSpecifier::Prefix("/".to_string())),
                            ..Default::default()
                        }),
                        action: Some(Action::Route(RouteAction {
                            cluster_specifier: Some(ClusterSpecifier::Cluster(
                                cluster_name.to_string(),
                            )),
                            ..Default::default()
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            })),
            ..Default::default()
        };
        let listener = Listener {
            name: listener_name.to_string(),
            api_listener: Some(ApiListener {
                api_listener: Some(Any {
                    type_url: TYPE_HCM.to_string(),
                    value: hcm.encode_to_vec().into(),
                }),
            }),
            ..Default::default()
        };
        resources
            .entry(TYPE_LISTENER.to_string())
            .or_default()
            .push(Any {
                type_url: TYPE_LISTENER.to_string(),
                value: listener.encode_to_vec(),
            });

        // CDS: Cluster (EDS type)
        let cluster = Cluster {
            name: cluster_name.to_string(),
            cluster_discovery_type: Some(
                envoy_types::pb::envoy::config::cluster::v3::cluster::ClusterDiscoveryType::Type(
                    DiscoveryType::Eds as i32,
                ),
            ),
            ..Default::default()
        };
        resources
            .entry(TYPE_CLUSTER.to_string())
            .or_default()
            .push(Any {
                type_url: TYPE_CLUSTER.to_string(),
                value: cluster.encode_to_vec(),
            });

        // EDS: ClusterLoadAssignment
        let cla = ClusterLoadAssignment {
            cluster_name: cluster_name.to_string(),
            endpoints: vec![LocalityLbEndpoints {
                lb_endpoints: endpoints
                    .iter()
                    .map(|(host, port)| LbEndpoint {
                        host_identifier: Some(HostIdentifier::Endpoint(
                            envoy_types::pb::envoy::config::endpoint::v3::Endpoint {
                                address: Some(core_v3::Address {
                                    address: Some(core_v3::address::Address::SocketAddress(
                                        core_v3::SocketAddress {
                                            address: host.clone(),
                                            port_specifier: Some(
                                                core_v3::socket_address::PortSpecifier::PortValue(
                                                    *port,
                                                ),
                                            ),
                                            ..Default::default()
                                        },
                                    )),
                                }),
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        };
        resources
            .entry(TYPE_ENDPOINTS.to_string())
            .or_default()
            .push(Any {
                type_url: TYPE_ENDPOINTS.to_string(),
                value: cla.encode_to_vec(),
            });

        Self { resources }
    }

    fn get(&self, type_url: &str) -> Vec<Any> {
        self.resources.get(type_url).cloned().unwrap_or_default()
    }
}

struct XdsServer {
    snapshot: Arc<Snapshot>,
}

#[tonic::async_trait]
impl AggregatedDiscoveryService for XdsServer {
    type StreamAggregatedResourcesStream =
        Pin<Box<dyn Stream<Item = Result<DiscoveryResponse, Status>> + Send>>;

    async fn stream_aggregated_resources(
        &self,
        request: Request<tonic::Streaming<DiscoveryRequest>>,
    ) -> Result<Response<Self::StreamAggregatedResourcesStream>, Status> {
        let mut inbound = request.into_inner();
        let snapshot = self.snapshot.clone();

        let outbound = async_stream::try_stream! {
            while let Some(req) = inbound.next().await {
                let req = req?;
                let short_type = req.type_url.rsplit('/').next().unwrap_or(&req.type_url);

                // Skip ACKs — only respond to new subscriptions or NACKs.
                if !req.version_info.is_empty() && req.error_detail.is_none() {
                    continue;
                }

                let resources = snapshot.get(&req.type_url);
                println!(
                    "  -> {short_type}: {count} resource(s)",
                    count = resources.len(),
                );
                yield DiscoveryResponse {
                    version_info: "1".to_string(),
                    type_url: req.type_url,
                    nonce: "1".to_string(),
                    resources,
                    ..Default::default()
                };
            }
        };

        Ok(Response::new(Box::pin(outbound)))
    }

    type DeltaAggregatedResourcesStream =
        Pin<Box<dyn Stream<Item = Result<DeltaDiscoveryResponse, Status>> + Send>>;

    async fn delta_aggregated_resources(
        &self,
        _request: Request<tonic::Streaming<DeltaDiscoveryRequest>>,
    ) -> Result<Response<Self::DeltaAggregatedResourcesStream>, Status> {
        Err(Status::unimplemented("delta not supported"))
    }
}

fn parse_endpoints(s: &str) -> Vec<(String, u32)> {
    s.split(',')
        .filter(|e| !e.is_empty())
        .map(|e| {
            let (host, port) = e.rsplit_once(':').expect("endpoint must be host:port");
            (host.to_string(), port.parse().expect("invalid port"))
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener_name = std::env::var("LISTENER_NAME").unwrap_or_else(|_| "my-service".to_string());
    let cluster_name = std::env::var("CLUSTER_NAME").unwrap_or_else(|_| "my-cluster".to_string());
    let endpoints_str =
        std::env::var("ENDPOINTS").unwrap_or_else(|_| "127.0.0.1:50051".to_string());
    let port = std::env::var("PORT").unwrap_or_else(|_| "18000".to_string());

    let endpoints = parse_endpoints(&endpoints_str);
    let snapshot = Arc::new(Snapshot::build(&listener_name, &cluster_name, &endpoints));

    let addr: std::net::SocketAddr = format!("0.0.0.0:{port}").parse()?;

    println!("xDS server listening on {addr}");
    println!("  listener: {listener_name}");
    println!("  cluster:  {cluster_name}");
    println!("  endpoints: {endpoints_str}");
    println!();

    tonic::transport::Server::builder()
        .add_service(AggregatedDiscoveryServiceServer::new(XdsServer {
            snapshot,
        }))
        .serve(addr)
        .await?;

    Ok(())
}
