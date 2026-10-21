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

use std::sync::Arc;

use protobuf::Parse;
use protobuf_well_known_types::Any;
use xds_client::Error;
use xds_client::Resource;
use xds_client::resource::TypeUrl;

use super::route::RouteConfigResource;
use crate::generated::envoy::config::listener::v3::Listener;
use crate::generated::envoy::extensions::filters::network::http_connection_manager::v3::{
    HttpConnectionManager, http_connection_manager::RouteSpecifierOneof,
};

/// The only `api_listener` extension gRPC supports.
const HTTP_CONNECTION_MANAGER_TYPE_URL: &str = "type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager";

/// How the listener obtains its route configuration.
#[derive(Debug, Clone)]
pub(crate) enum RouteSource {
    /// Route configuration fetched dynamically via RDS, keyed by this route
    /// config name.
    Rds(String),
    /// Route configuration embedded inline in the listener.
    Inline(Arc<RouteConfigResource>),
}

/// Validated Listener resource.
///
/// Extracts the route source from the
/// `ApiListener` -> `HttpConnectionManager` -> `route_specifier` chain per
/// gRFC A27. `scoped_routes` is not supported, matching other gRPC xDS
/// client implementation.
#[derive(Debug, Clone)]
pub(crate) struct ListenerResource {
    pub(crate) name: String,
    pub(crate) route_source: RouteSource,
}

impl Resource for ListenerResource {
    type Message = Listener;

    const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.listener.v3.Listener");

    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = true;

    fn deserialize(bytes: bytes::Bytes) -> xds_client::Result<Self::Message> {
        Listener::parse(&bytes)
            .map_err(|e| Error::Validation(format!("failed to decode Listener: {e}")))
    }

    fn name(message: &Self::Message) -> &str {
        message.name().to_str().unwrap_or_default()
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let name = message.name().to_str().unwrap_or_default().to_string();
        if name.is_empty() {
            return Err(Error::Validation("listener name is empty".into()));
        }

        if !message.has_api_listener() {
            return Err(Error::Validation(
                "listener missing api_listener field".into(),
            ));
        }
        let api_listener = message.api_listener();

        if !api_listener.has_api_listener() {
            return Err(Error::Validation(
                "api_listener missing inner api_listener Any field".into(),
            ));
        }
        let any: Any = api_listener.api_listener().to_owned();

        let type_url = any.type_url().to_str().unwrap_or_default();
        if type_url != HTTP_CONNECTION_MANAGER_TYPE_URL {
            return Err(Error::Validation(format!(
                "unexpected api_listener type_url: '{type_url}'"
            )));
        }

        let hcm = HttpConnectionManager::parse(any.value()).map_err(|e| {
            Error::Validation(format!("failed to decode HttpConnectionManager: {e}"))
        })?;

        let route_source = match hcm.route_specifier() {
            RouteSpecifierOneof::Rds(rds) => {
                if !rds.has_config_source() {
                    return Err(Error::Validation("RDS config_source is not set".into()));
                }
                let config_source = rds.config_source();
                if !config_source.has_ads() && !config_source.has_self() {
                    return Err(Error::Validation(
                        "RDS config_source is not ADS or Self".into(),
                    ));
                }

                let route_config_name = rds.route_config_name().to_str().unwrap_or_default();
                if route_config_name.is_empty() {
                    return Err(Error::Validation("RDS route_config_name is empty".into()));
                }
                RouteSource::Rds(route_config_name.to_string())
            }
            RouteSpecifierOneof::RouteConfig(route_config) => {
                let validated = Arc::new(RouteConfigResource::validate(route_config.to_owned())?);
                RouteSource::Inline(validated)
            }
            RouteSpecifierOneof::ScopedRoutes(_) => {
                return Err(Error::Validation(
                    "scoped_routes not supported for gRPC".into(),
                ));
            }
            RouteSpecifierOneof::not_set(_) => {
                return Err(Error::Validation(
                    "HttpConnectionManager missing route_specifier".into(),
                ));
            }
        };

        Ok(ListenerResource { name, route_source })
    }
}

impl ListenerResource {
    /// Returns the RDS route config name for cascading subscriptions, or
    /// `None` when the route configuration was embedded inline.
    pub(crate) fn route_config_name(&self) -> Option<&str> {
        match &self.route_source {
            RouteSource::Rds(name) => Some(name),
            RouteSource::Inline(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::envoy::config::core::v3::AggregatedConfigSource;
    use crate::generated::envoy::config::core::v3::ConfigSource;
    use crate::generated::envoy::config::core::v3::SelfConfigSource;
    use crate::generated::envoy::config::listener::v3::ApiListener;
    use crate::generated::envoy::config::route::v3::Route;
    use crate::generated::envoy::config::route::v3::RouteAction;
    use crate::generated::envoy::config::route::v3::RouteConfiguration;
    use crate::generated::envoy::config::route::v3::RouteMatch;
    use crate::generated::envoy::config::route::v3::VirtualHost;
    use crate::generated::envoy::extensions::filters::network::http_connection_manager::v3::Rds;
    use protobuf::Serialize;

    const HCM_TYPE_URL: &str = "type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager";

    fn wrap_hcm(hcm: &HttpConnectionManager) -> Any {
        let mut any = Any::new();
        any.set_type_url(HCM_TYPE_URL);
        any.set_value(hcm.serialize().expect("serialize hcm"));
        any
    }

    fn make_rds_listener_with_config_source(
        name: &str,
        route_config_name: &str,
        config_source: Option<ConfigSource>,
    ) -> Listener {
        let mut rds = Rds::new();
        rds.set_route_config_name(route_config_name);
        if let Some(config_source) = config_source {
            rds.set_config_source(config_source);
        }
        let mut hcm = HttpConnectionManager::new();
        hcm.set_rds(rds);

        let mut api_listener = ApiListener::new();
        api_listener.set_api_listener(wrap_hcm(&hcm));

        let mut listener = Listener::new();
        listener.set_name(name);
        listener.set_api_listener(api_listener);
        listener
    }

    fn make_rds_listener(name: &str, route_config_name: &str) -> Listener {
        let mut config_source = ConfigSource::new();
        config_source.set_ads(AggregatedConfigSource::new());
        make_rds_listener_with_config_source(name, route_config_name, Some(config_source))
    }

    #[test]
    fn validate_rds_listener() {
        let listener = make_rds_listener("test-listener", "route-config-1");
        let validated = ListenerResource::validate(listener).expect("should validate");
        assert_eq!(validated.name, "test-listener");
        assert!(
            matches!(&validated.route_source, RouteSource::Rds(name) if name == "route-config-1")
        );
        assert_eq!(validated.route_config_name(), Some("route-config-1"));
    }

    #[test]
    fn validate_missing_api_listener() {
        let mut listener = Listener::new();
        listener.set_name("test-listener");
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("api_listener"));
    }

    #[test]
    fn validate_rejects_empty_listener_name() {
        let listener = make_rds_listener("", "route-config-1");
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("listener name is empty"));
    }

    #[test]
    fn validate_rejects_unexpected_api_listener_type_url() {
        let mut listener = make_rds_listener("test-listener", "route-config-1");
        let mut any = listener.api_listener().api_listener().to_owned();
        any.set_type_url("type.googleapis.com/envoy.config.listener.v3.Listener");
        let mut api_listener = ApiListener::new();
        api_listener.set_api_listener(any);
        listener.set_api_listener(api_listener);

        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("unexpected api_listener type_url"));
    }

    #[test]
    fn validate_empty_rds_name() {
        let listener = make_rds_listener("test-listener", "");
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("route_config_name is empty"));
    }

    #[test]
    fn validate_rds_listener_accepts_self_config_source() {
        let mut config_source = ConfigSource::new();
        config_source.set_self(SelfConfigSource::new());
        let listener = make_rds_listener_with_config_source(
            "test-listener",
            "route-config-1",
            Some(config_source),
        );
        assert!(ListenerResource::validate(listener).is_ok());
    }

    #[test]
    fn validate_rds_listener_rejects_missing_config_source() {
        let listener =
            make_rds_listener_with_config_source("test-listener", "route-config-1", None);
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("config_source is not set"));
    }

    #[test]
    fn validate_rds_listener_rejects_non_ads_non_self_config_source() {
        let mut config_source = ConfigSource::new();
        config_source.set_path("/some/path");
        let listener = make_rds_listener_with_config_source(
            "test-listener",
            "route-config-1",
            Some(config_source),
        );
        let err = ListenerResource::validate(listener).unwrap_err();
        assert!(err.to_string().contains("not ADS or Self"));
    }

    #[test]
    fn deserialize_valid() {
        let listener = make_rds_listener("test", "rc1");
        let bytes = listener.serialize().expect("serialize");
        let deserialized =
            ListenerResource::deserialize(bytes::Bytes::from(bytes)).expect("should deserialize");
        assert_eq!(ListenerResource::name(&deserialized), "test");
    }

    #[test]
    fn deserialize_invalid_bytes() {
        // A lone 0x80 is not a valid protobuf varint tag: this should fail
        // decoding rather than silently produce a default message.
        let result = ListenerResource::deserialize(bytes::Bytes::from_static(b"\x80"));
        assert!(result.is_err());
    }

    #[test]
    fn validate_inline_route_config() {
        let mut route_match = RouteMatch::new();
        route_match.set_prefix("/");
        let mut route_action = RouteAction::new();
        route_action.set_cluster("cluster-1");
        let mut route = Route::new();
        route.set_match(route_match);
        route.set_route(route_action);

        let mut vh = VirtualHost::new();
        vh.set_name("vh1");
        vh.domains_mut().push("*");
        vh.routes_mut().push(route);

        let mut route_config = RouteConfiguration::new();
        route_config.set_name("inline-rc");
        route_config.virtual_hosts_mut().push(vh);

        let mut hcm = HttpConnectionManager::new();
        hcm.set_route_config(route_config);

        let mut api_listener = ApiListener::new();
        api_listener.set_api_listener(wrap_hcm(&hcm));

        let mut listener = Listener::new();
        listener.set_name("inline-listener");
        listener.set_api_listener(api_listener);

        let validated = ListenerResource::validate(listener).expect("should validate");
        assert_eq!(validated.name, "inline-listener");
        assert!(matches!(&validated.route_source, RouteSource::Inline(_)));
        assert!(validated.route_config_name().is_none());

        let RouteSource::Inline(rc) = &validated.route_source else {
            unreachable!()
        };
        assert_eq!(rc.virtual_hosts.len(), 1);
    }
}
