/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! Validated Listener resource (LDS).

use std::time::Duration;

use bytes::Bytes;
use envoy_types::pb::envoy::config::listener::v3::Listener;
use envoy_types::pb::envoy::extensions::filters::network::http_connection_manager::v3::{
    HttpConnectionManager, http_connection_manager::RouteSpecifier,
};
use prost::Message;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

use super::proto_duration;
use super::route_config::RouteConfigResource;

/// How the listener obtains its route configuration.
#[derive(Debug)]
pub(crate) enum RouteSource {
    /// Route configuration fetched dynamically via RDS.
    Rds(String),
    /// Route configuration embedded inline in the listener.
    Inline(RouteConfigResource),
}

/// Validated Listener resource.
///
/// Extracts the route source from the
/// `ApiListener` -> `HttpConnectionManager` -> `route_specifier` chain.
#[derive(Debug)]
pub(crate) struct ListenerResource {
    pub name: String,
    pub route_source: RouteSource,
    /// gRFC A31 default max stream duration from
    /// `HttpConnectionManager.common_http_protocol_options.max_stream_duration`,
    /// applied to routes that set no `RouteAction.max_stream_duration`. `None`
    /// when unset; `Some(ZERO)` means no limit.
    pub max_stream_duration: Option<Duration>,
}

impl Resource for ListenerResource {
    type Message = Listener;

    const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.listener.v3.Listener");

    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = true;

    fn deserialize(bytes: Bytes) -> xds_client::Result<Self::Message> {
        Listener::decode(bytes).map_err(Into::into)
    }

    fn name(message: &Self::Message) -> &str {
        &message.name
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let name = message.name;

        // gRPC listeners must have an ApiListener.
        let api_listener = message
            .api_listener
            .ok_or_else(|| Error::Validation("listener missing api_listener field".into()))?;

        // The ApiListener contains an Any that should be HttpConnectionManager.
        let any = api_listener.api_listener.ok_or_else(|| {
            Error::Validation("api_listener missing inner api_listener Any field".into())
        })?;

        let hcm = HttpConnectionManager::decode(Bytes::from(any.value)).map_err(|e| {
            Error::Validation(format!("failed to decode HttpConnectionManager: {e}"))
        })?;

        let max_stream_duration = hcm
            .common_http_protocol_options
            .as_ref()
            .and_then(|opts| opts.max_stream_duration.as_ref())
            .map(|d| {
                proto_duration(d).ok_or_else(|| {
                    Error::Validation(
                        "common_http_protocol_options.max_stream_duration is not a valid duration"
                            .into(),
                    )
                })
            })
            .transpose()?;

        let route_specifier = hcm.route_specifier.ok_or_else(|| {
            Error::Validation("HttpConnectionManager missing route_specifier".into())
        })?;

        match route_specifier {
            RouteSpecifier::Rds(rds) => {
                if rds.route_config_name.is_empty() {
                    return Err(Error::Validation("RDS route_config_name is empty".into()));
                }
                Ok(ListenerResource {
                    name,
                    route_source: RouteSource::Rds(rds.route_config_name),
                    max_stream_duration,
                })
            }
            RouteSpecifier::RouteConfig(route_config) => {
                let validated = RouteConfigResource::validate(route_config)?;
                Ok(ListenerResource {
                    name,
                    route_source: RouteSource::Inline(validated),
                    max_stream_duration,
                })
            }
            RouteSpecifier::ScopedRoutes(_) => Err(Error::Validation(
                "scoped_routes not supported for gRPC".into(),
            )),
        }
    }
}

impl ListenerResource {
    /// Returns the RDS route config name for cascading subscriptions.
    pub(crate) fn route_config_name(&self) -> Option<&str> {
        match &self.route_source {
            RouteSource::Rds(name) => Some(name),
            RouteSource::Inline(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use envoy_types::pb::envoy::config::listener::v3::ApiListener;
    use envoy_types::pb::envoy::extensions::filters::network::http_connection_manager::v3::Rds;
    use envoy_types::pb::google::protobuf::Any;

    use super::*;

    fn make_rds_listener(name: &str, route_config_name: &str) -> Listener {
        make_rds_listener_with_hcm(name, route_config_name, HttpConnectionManager::default())
    }

    fn make_rds_listener_with_hcm(
        name: &str,
        route_config_name: &str,
        hcm: HttpConnectionManager,
    ) -> Listener {
        let rds = Rds {
            route_config_name: route_config_name.to_string(),
            ..Default::default()
        };
        let hcm = HttpConnectionManager {
            route_specifier: Some(RouteSpecifier::Rds(rds)),
            ..hcm
        };
        let hcm_any = Any {
            type_url: "type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager".to_string(),
            value: hcm.encode_to_vec(),
        };
        Listener {
            name: name.to_string(),
            api_listener: Some(ApiListener {
                api_listener: Some(hcm_any),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_validate_rds_listener() {
        let listener = make_rds_listener("test-listener", "route-config-1");
        let validated = ListenerResource::validate(listener).expect("should validate");
        assert_eq!(validated.name, "test-listener");
        assert!(
            matches!(&validated.route_source, RouteSource::Rds(name) if name == "route-config-1")
        );
        assert_eq!(validated.route_config_name(), Some("route-config-1"));
    }

    #[test]
    fn test_validate_missing_api_listener() {
        let listener = Listener {
            name: "test-listener".to_string(),
            ..Default::default()
        };
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("api_listener"));
    }

    #[test]
    fn test_validate_empty_rds_name() {
        let listener = make_rds_listener("test-listener", "");
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("route_config_name is empty"));
    }

    #[test]
    fn test_deserialize_valid() {
        let listener = make_rds_listener("test", "rc1");
        let bytes = listener.encode_to_vec();
        let deserialized =
            ListenerResource::deserialize(Bytes::from(bytes)).expect("should deserialize");
        assert_eq!(ListenerResource::name(&deserialized), "test");
    }

    #[test]
    fn test_deserialize_invalid_bytes() {
        let result = ListenerResource::deserialize(Bytes::from_static(b"invalid"));
        assert!(result.is_err());
    }

    #[test]
    fn test_type_url() {
        assert_eq!(
            ListenerResource::TYPE_URL.as_str(),
            "type.googleapis.com/envoy.config.listener.v3.Listener"
        );
    }

    #[test]
    fn test_validate_inline_route_config() {
        use envoy_types::pb::envoy::config::route::v3::route_match::PathSpecifier;
        use envoy_types::pb::envoy::config::route::v3::{
            RouteAction, RouteConfiguration, RouteMatch, VirtualHost, route::Action,
        };

        let route_config = RouteConfiguration {
            name: "inline-rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![envoy_types::pb::envoy::config::route::v3::Route {
                    r#match: Some(RouteMatch {
                        path_specifier: Some(PathSpecifier::Prefix("/".to_string())),
                        ..Default::default()
                    }),
                    action: Some(Action::Route(RouteAction {
                        cluster_specifier: Some(
                            envoy_types::pb::envoy::config::route::v3::route_action::ClusterSpecifier::Cluster(
                                "cluster-1".to_string(),
                            ),
                        ),
                        ..Default::default()
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let hcm = HttpConnectionManager {
            route_specifier: Some(RouteSpecifier::RouteConfig(route_config)),
            ..Default::default()
        };
        let hcm_any = Any {
            type_url: "type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager".to_string(),
            value: hcm.encode_to_vec(),
        };
        let listener = Listener {
            name: "inline-listener".to_string(),
            api_listener: Some(ApiListener {
                api_listener: Some(hcm_any),
            }),
            ..Default::default()
        };

        let validated = ListenerResource::validate(listener).expect("should validate");
        assert_eq!(validated.name, "inline-listener");
        assert!(matches!(&validated.route_source, RouteSource::Inline(_)));
        assert!(validated.route_config_name().is_none());
    }

    fn hcm_with_max_stream_duration(seconds: i64, nanos: i32) -> HttpConnectionManager {
        use envoy_types::pb::envoy::config::core::v3::HttpProtocolOptions;
        use envoy_types::pb::google::protobuf::Duration as ProtoDuration;
        HttpConnectionManager {
            common_http_protocol_options: Some(HttpProtocolOptions {
                max_stream_duration: Some(ProtoDuration { seconds, nanos }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_max_stream_duration_parsed_from_hcm() {
        let validated =
            ListenerResource::validate(make_rds_listener("l", "rc")).expect("should validate");
        assert_eq!(validated.max_stream_duration, None);

        // Zero ("no limit") is kept rather than collapsed to unset.
        for (seconds, want) in [(2, Duration::from_secs(2)), (0, Duration::ZERO)] {
            let hcm = hcm_with_max_stream_duration(seconds, 0);
            let listener = make_rds_listener_with_hcm("l", "rc", hcm);
            let validated = ListenerResource::validate(listener).expect("should validate");
            assert_eq!(validated.max_stream_duration, Some(want));
        }
    }

    #[test]
    fn test_invalid_max_stream_duration_is_rejected() {
        let listener = make_rds_listener_with_hcm("l", "rc", hcm_with_max_stream_duration(-1, 0));
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("max_stream_duration"), "{err}");
    }
}
