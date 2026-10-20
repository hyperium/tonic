/*
 *
 * Copyright 2026 gRPC authors.
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

//! HTTP filter registry and config unwrapping ([gRFC A39]).
//!
//! This module parses a single HCM [`HttpFilter`] or RDS
//! `typed_per_filter_config` `Any`. It does not walk the filter list, merge
//! overrides, or install data-plane interceptors.
//!
//! [gRFC A39]: https://github.com/grpc/proposal/blob/master/A39-xds-http-filters.md

use std::collections::HashMap;

use envoy_types::pb::envoy::config::route::v3::FilterConfig;
use envoy_types::pb::envoy::extensions::filters::network::http_connection_manager::v3::{
    HttpFilter, http_filter::ConfigType,
};
use envoy_types::pb::google::protobuf::Any;
use prost::Message;
use xds_client::Error;

/// Proto type name for the router filter (gRFC A39 terminal filter).
pub(crate) const ROUTER_TYPE_NAME: &str = "envoy.extensions.filters.http.router.v3.Router";

const FILTER_CONFIG_TYPE_NAME: &str = "envoy.config.route.v3.FilterConfig";
const TYPED_STRUCT_XDS_TYPE_NAME: &str = "xds.type.v3.TypedStruct";
const TYPED_STRUCT_UDPA_TYPE_NAME: &str = "udpa.type.v1.TypedStruct";

/// Validated filter configuration produced by a [`RegisteredFilter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidatedFilterConfig {
    /// `envoy.extensions.filters.http.router.v3.Router`. All proto fields are
    /// ignored per A39.
    Router,
}

/// Result of parsing one RDS `typed_per_filter_config` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverrideConfig {
    /// `FilterConfig.disabled = true`: do not run this instance on the route.
    Disabled,
    /// Validated override config for a registered filter.
    Config(ValidatedFilterConfig),
}

/// One accepted HCM `http_filters` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpFilterInstance {
    /// Filter instance name. Overrides are keyed by this name.
    pub name: String,
    /// Validated top-level config.
    pub config: ValidatedFilterConfig,
    /// Whether this filter must be last in the HCM list.
    pub is_terminal: bool,
}

/// A registered xDS HTTP filter implementation.
pub(crate) trait RegisteredFilter: Send + Sync + std::fmt::Debug {
    /// Canonical proto type name, without a `type.googleapis.com/` prefix.
    fn type_name(&self) -> &'static str;

    /// Whether this filter must be the last entry in `http_filters`.
    fn is_terminal(&self) -> bool;

    /// Validate an HCM top-level `typed_config` payload.
    fn validate_top_level(&self, value: &[u8]) -> xds_client::Result<ValidatedFilterConfig>;

    /// Validate an RDS `typed_per_filter_config` override payload.
    ///
    /// The default rejects overrides: filters that do not define an override
    /// message NACK rather than silently ignore.
    fn validate_override(&self, _value: &[u8]) -> xds_client::Result<ValidatedFilterConfig> {
        Err(Error::Validation(format!(
            "HTTP filter '{}' does not support override config",
            self.type_name()
        )))
    }
}

/// Registry of HTTP filters keyed by proto type name.
#[derive(Debug)]
pub(crate) struct HttpFilterRegistry {
    filters: HashMap<&'static str, Box<dyn RegisteredFilter>>,
}

impl HttpFilterRegistry {
    /// Client-side registry. Currently only the router filter is registered.
    pub(crate) fn client() -> Self {
        let mut registry = Self {
            filters: HashMap::new(),
        };
        registry.register(Box::new(RouterFilter));
        registry
    }

    fn register(&mut self, filter: Box<dyn RegisteredFilter>) {
        self.filters.insert(filter.type_name(), filter);
    }

    fn get(&self, type_name: &str) -> Option<&dyn RegisteredFilter> {
        self.filters.get(type_name).map(|filter| filter.as_ref())
    }
}

/// Parse one HCM [`HttpFilter`].
///
/// Returns `Ok(None)` when the filter is unknown and marked optional, so the
/// caller drops it. Any other failure is a validation error and must NACK.
///
/// A39 puts `FilterConfig` on RDS overrides, not HCM `http_filters`. A
/// `FilterConfig` type URL here is treated as an unknown filter type.
pub(crate) fn parse_http_filter(
    filter: &HttpFilter,
    registry: &HttpFilterRegistry,
) -> xds_client::Result<Option<HttpFilterInstance>> {
    if filter.name.is_empty() {
        return Err(Error::Validation("HTTP filter is missing a name".into()));
    }
    if filter.disabled {
        return Err(Error::Validation(format!(
            "HTTP filter '{}' sets disabled=true, which is not supported",
            filter.name
        )));
    }

    let Some(config_type) = filter.config_type.as_ref() else {
        return Err(Error::Validation(format!(
            "HTTP filter '{}' is missing typed_config",
            filter.name
        )));
    };
    let ConfigType::TypedConfig(any) = config_type else {
        return Err(Error::Validation(format!(
            "HTTP filter '{}' uses ConfigDiscovery, which is not supported",
            filter.name
        )));
    };

    let Some((type_name, value, is_optional)) = unwrap_typed_struct(any, filter.is_optional)?
    else {
        return Ok(None);
    };
    lookup_top_level(filter, registry, &type_name, &value, is_optional)
}

fn lookup_top_level(
    filter: &HttpFilter,
    registry: &HttpFilterRegistry,
    type_name: &str,
    value: &[u8],
    is_optional: bool,
) -> xds_client::Result<Option<HttpFilterInstance>> {
    let Some(registered) = registry.get(type_name) else {
        if is_optional {
            return Ok(None);
        }
        return Err(Error::Validation(format!(
            "HTTP filter '{}' has unknown type '{type_name}'",
            filter.name
        )));
    };

    let config = registered.validate_top_level(value)?;
    Ok(Some(HttpFilterInstance {
        name: filter.name.clone(),
        config,
        is_terminal: registered.is_terminal(),
    }))
}

/// Parse one RDS `typed_per_filter_config` value.
///
/// Returns `Ok(None)` when the override is unknown and optional.
pub(crate) fn parse_override_config(
    any: &Any,
    registry: &HttpFilterRegistry,
) -> xds_client::Result<Option<OverrideConfig>> {
    match unwrap_override(any)? {
        None => Ok(None),
        Some(UnwrappedOverride::Disabled) => Ok(Some(OverrideConfig::Disabled)),
        Some(UnwrappedOverride::Payload {
            type_name,
            value,
            is_optional,
        }) => {
            let Some(registered) = registry.get(&type_name) else {
                if is_optional {
                    return Ok(None);
                }
                return Err(Error::Validation(format!(
                    "HTTP filter override has unknown type '{type_name}'"
                )));
            };
            Ok(Some(OverrideConfig::Config(
                registered.validate_override(&value)?,
            )))
        }
    }
}

enum UnwrappedOverride {
    Disabled,
    Payload {
        type_name: String,
        value: Vec<u8>,
        is_optional: bool,
    },
}

/// A39: at most one `FilterConfig` wrapper, then at most one `TypedStruct`.
fn unwrap_override(any: &Any) -> xds_client::Result<Option<UnwrappedOverride>> {
    let type_name = proto_type_name(&any.type_url);
    if type_name != FILTER_CONFIG_TYPE_NAME {
        return Ok(
            unwrap_typed_struct(any, false)?.map(|(type_name, value, is_optional)| {
                UnwrappedOverride::Payload {
                    type_name,
                    value,
                    is_optional,
                }
            }),
        );
    }

    let wrapper = FilterConfig::decode(any.value.as_slice())
        .map_err(|e| Error::Validation(format!("failed to decode FilterConfig wrapper: {e}")))?;
    if wrapper.disabled {
        return Ok(Some(UnwrappedOverride::Disabled));
    }
    let is_optional = wrapper.is_optional;
    let Some(inner) = wrapper.config.as_ref() else {
        if is_optional {
            return Ok(None);
        }
        return Err(Error::Validation(
            "FilterConfig wrapper is missing config".into(),
        ));
    };
    if proto_type_name(&inner.type_url) == FILTER_CONFIG_TYPE_NAME {
        return Err(Error::Validation(
            "nested FilterConfig wrappers are not supported".into(),
        ));
    }
    Ok(
        unwrap_typed_struct(inner, is_optional)?.map(|(type_name, value, is_optional)| {
            UnwrappedOverride::Payload {
                type_name,
                value,
                is_optional,
            }
        }),
    )
}

/// Unwrap one `TypedStruct` layer, or return the native type URL and bytes.
///
/// TypedStruct field 2 is `google.protobuf.Struct`; native proto bytes are not
/// recovered here. Filters that ignore config (router) still ACK.
fn unwrap_typed_struct(
    any: &Any,
    is_optional: bool,
) -> xds_client::Result<Option<(String, Vec<u8>, bool)>> {
    let type_name = proto_type_name(&any.type_url);
    if type_name == TYPED_STRUCT_XDS_TYPE_NAME || type_name == TYPED_STRUCT_UDPA_TYPE_NAME {
        let typed = TypedStruct::decode(any.value.as_slice())
            .map_err(|e| Error::Validation(format!("failed to decode TypedStruct wrapper: {e}")))?;
        if typed.type_url.is_empty() {
            return Err(Error::Validation("TypedStruct is missing type_url".into()));
        }
        let inner_name = proto_type_name(&typed.type_url);
        if inner_name == FILTER_CONFIG_TYPE_NAME
            || inner_name == TYPED_STRUCT_XDS_TYPE_NAME
            || inner_name == TYPED_STRUCT_UDPA_TYPE_NAME
        {
            return Err(Error::Validation(
                "nested TypedStruct / FilterConfig wrappers are not supported".into(),
            ));
        }
        return Ok(Some((inner_name.to_string(), Vec::new(), is_optional)));
    }
    if type_name.is_empty() {
        return Err(Error::Validation(
            "filter config Any is missing type_url".into(),
        ));
    }
    Ok(Some((
        type_name.to_string(),
        any.value.clone(),
        is_optional,
    )))
}

fn proto_type_name(type_url: &str) -> &str {
    type_url.rsplit('/').next().unwrap_or(type_url)
}

#[derive(Debug)]
struct RouterFilter;

impl RegisteredFilter for RouterFilter {
    fn type_name(&self) -> &'static str {
        ROUTER_TYPE_NAME
    }

    fn is_terminal(&self) -> bool {
        true
    }

    fn validate_top_level(&self, _value: &[u8]) -> xds_client::Result<ValidatedFilterConfig> {
        // A39: accept the router filter and ignore every field.
        Ok(ValidatedFilterConfig::Router)
    }
}

/// Wire-compatible `xds.type.v3.TypedStruct` / `udpa.type.v1.TypedStruct`.
///
/// Only `type_url` is decoded. Field 2 (`google.protobuf.Struct value`) is
/// ignored until a filter needs native Struct decoding.
#[derive(Clone, PartialEq, Message)]
struct TypedStruct {
    #[prost(string, tag = "1")]
    type_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::extensions::filters::http::router::v3::Router;
    use prost::Name;

    fn registry() -> HttpFilterRegistry {
        HttpFilterRegistry::client()
    }

    fn any_of<M: Message + Name>(msg: &M) -> Any {
        Any {
            type_url: M::type_url(),
            value: msg.encode_to_vec(),
        }
    }

    fn router_any() -> Any {
        any_of(&Router::default())
    }

    fn fault_any() -> Any {
        Any {
            type_url: "type.googleapis.com/envoy.extensions.filters.http.fault.v3.HTTPFault".into(),
            value: vec![],
        }
    }

    fn http_filter(name: &str, any: Any, is_optional: bool) -> HttpFilter {
        HttpFilter {
            name: name.to_string(),
            is_optional,
            config_type: Some(ConfigType::TypedConfig(any)),
            ..Default::default()
        }
    }

    fn typed_struct_any(type_url: &str, type_name: &str) -> Any {
        Any {
            type_url: format!("type.googleapis.com/{type_name}"),
            value: TypedStruct {
                type_url: type_url.to_string(),
            }
            .encode_to_vec(),
        }
    }

    fn filter_config(config: Option<Any>, is_optional: bool, disabled: bool) -> Any {
        any_of(&FilterConfig {
            config,
            is_optional,
            disabled,
        })
    }

    #[test]
    fn router_top_level_is_accepted() {
        let parsed = parse_http_filter(&http_filter("router", router_any(), false), &registry())
            .expect("router should validate")
            .expect("router should not be dropped");
        assert_eq!(parsed.name, "router");
        assert_eq!(parsed.config, ValidatedFilterConfig::Router);
        assert!(parsed.is_terminal);
    }

    #[test]
    fn optional_known_router_is_not_dropped() {
        let parsed = parse_http_filter(&http_filter("router", router_any(), true), &registry())
            .unwrap()
            .unwrap();
        assert_eq!(parsed.config, ValidatedFilterConfig::Router);
    }

    #[test]
    fn router_fields_are_ignored() {
        let router = Router {
            suppress_envoy_headers: true,
            ..Default::default()
        };
        let parsed = parse_http_filter(&http_filter("router", any_of(&router), false), &registry())
            .unwrap()
            .unwrap();
        assert_eq!(parsed.config, ValidatedFilterConfig::Router);
    }

    #[test]
    fn unknown_required_type_nacks() {
        let err =
            parse_http_filter(&http_filter("fault", fault_any(), false), &registry()).unwrap_err();
        assert!(err.to_string().contains("unknown type"));
    }

    #[test]
    fn unknown_optional_type_is_dropped() {
        assert_eq!(
            parse_http_filter(&http_filter("fault", fault_any(), true), &registry()).unwrap(),
            None
        );
    }

    #[test]
    fn missing_name_nacks() {
        let err =
            parse_http_filter(&http_filter("", router_any(), false), &registry()).unwrap_err();
        assert!(err.to_string().contains("missing a name"));
    }

    #[test]
    fn missing_typed_config_nacks() {
        let filter = HttpFilter {
            name: "router".to_string(),
            ..Default::default()
        };
        let err = parse_http_filter(&filter, &registry()).unwrap_err();
        assert!(err.to_string().contains("missing typed_config"));
    }

    #[test]
    fn config_discovery_nacks() {
        use envoy_types::pb::envoy::config::core::v3::ExtensionConfigSource;

        let filter = HttpFilter {
            name: "router".to_string(),
            config_type: Some(ConfigType::ConfigDiscovery(ExtensionConfigSource::default())),
            ..Default::default()
        };
        let err = parse_http_filter(&filter, &registry()).unwrap_err();
        assert!(err.to_string().contains("ConfigDiscovery"));
    }

    #[test]
    fn disabled_hcm_filter_nacks() {
        let filter = HttpFilter {
            name: "router".to_string(),
            disabled: true,
            config_type: Some(ConfigType::TypedConfig(router_any())),
            ..Default::default()
        };
        let err = parse_http_filter(&filter, &registry()).unwrap_err();
        assert!(err.to_string().contains("disabled=true"));
    }

    #[test]
    fn xds_typed_struct_unwraps_router_type() {
        let any = typed_struct_any(
            &format!("type.googleapis.com/{ROUTER_TYPE_NAME}"),
            TYPED_STRUCT_XDS_TYPE_NAME,
        );
        let parsed = parse_http_filter(&http_filter("router", any, false), &registry())
            .unwrap()
            .unwrap();
        assert_eq!(parsed.config, ValidatedFilterConfig::Router);
    }

    #[test]
    fn udpa_typed_struct_unwraps_router_type() {
        let any = typed_struct_any(
            &format!("type.googleapis.com/{ROUTER_TYPE_NAME}"),
            TYPED_STRUCT_UDPA_TYPE_NAME,
        );
        let parsed = parse_http_filter(&http_filter("router", any, false), &registry())
            .unwrap()
            .unwrap();
        assert_eq!(parsed.config, ValidatedFilterConfig::Router);
    }

    #[test]
    fn udpa_typed_struct_unwraps_unknown_optional() {
        let any = typed_struct_any(
            "type.googleapis.com/envoy.extensions.filters.http.fault.v3.HTTPFault",
            TYPED_STRUCT_UDPA_TYPE_NAME,
        );
        assert_eq!(
            parse_http_filter(&http_filter("fault", any, true), &registry()).unwrap(),
            None
        );
    }

    #[test]
    fn typed_struct_unknown_required_nacks() {
        let any = typed_struct_any(
            "type.googleapis.com/envoy.extensions.filters.http.fault.v3.HTTPFault",
            TYPED_STRUCT_XDS_TYPE_NAME,
        );
        let err = parse_http_filter(&http_filter("fault", any, false), &registry()).unwrap_err();
        assert!(err.to_string().contains("unknown type"));
    }

    #[test]
    fn typed_struct_missing_type_url_nacks() {
        let any = typed_struct_any("", TYPED_STRUCT_XDS_TYPE_NAME);
        let err = parse_http_filter(&http_filter("router", any, false), &registry()).unwrap_err();
        assert!(err.to_string().contains("TypedStruct is missing type_url"));
    }

    #[test]
    fn hcm_filter_config_type_is_unknown() {
        let err = parse_http_filter(
            &http_filter(
                "router",
                filter_config(Some(router_any()), false, false),
                false,
            ),
            &registry(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown type"));
        assert!(err.to_string().contains(FILTER_CONFIG_TYPE_NAME));
    }

    #[test]
    fn override_filter_config_unwraps_then_nacks_router() {
        let err = parse_override_config(
            &filter_config(Some(router_any()), false, false),
            &registry(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not support override config"));
    }

    #[test]
    fn override_filter_config_optional_unknown_is_dropped() {
        assert_eq!(
            parse_override_config(&filter_config(Some(fault_any()), true, false), &registry())
                .unwrap(),
            None
        );
    }

    #[test]
    fn override_filter_config_required_unknown_nacks() {
        let err =
            parse_override_config(&filter_config(Some(fault_any()), false, false), &registry())
                .unwrap_err();
        assert!(err.to_string().contains("unknown type"));
    }

    #[test]
    fn override_filter_config_disabled_is_disabled() {
        assert_eq!(
            parse_override_config(&filter_config(None, false, true), &registry()).unwrap(),
            Some(OverrideConfig::Disabled)
        );
    }

    #[test]
    fn override_nested_filter_config_nacks() {
        let inner = filter_config(Some(router_any()), false, false);
        let err = parse_override_config(&filter_config(Some(inner), false, false), &registry())
            .unwrap_err();
        assert!(err.to_string().contains("nested FilterConfig"));
    }

    #[test]
    fn override_filter_config_typed_struct_router_nacks() {
        let typed = typed_struct_any(
            &format!("type.googleapis.com/{ROUTER_TYPE_NAME}"),
            TYPED_STRUCT_XDS_TYPE_NAME,
        );
        let err = parse_override_config(&filter_config(Some(typed), false, false), &registry())
            .unwrap_err();
        assert!(err.to_string().contains("does not support override config"));
    }

    #[test]
    fn router_override_nacks() {
        let err = parse_override_config(&router_any(), &registry()).unwrap_err();
        assert!(err.to_string().contains("does not support override config"));
    }

    #[test]
    fn required_unknown_override_nacks() {
        let err = parse_override_config(&fault_any(), &registry()).unwrap_err();
        assert!(err.to_string().contains("unknown type"));
    }
}
