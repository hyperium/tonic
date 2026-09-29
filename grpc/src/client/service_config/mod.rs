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

pub(crate) mod duration;
pub(crate) mod serde_bindings;

use std::sync::Arc;
use std::sync::LazyLock;

use crate::client::load_balancing::DynLbConfig;
use crate::client::load_balancing::DynLbPolicyBuilder;
use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
use crate::client::load_balancing::LbConfigJson;
use crate::client::load_balancing::pick_first;

static DEFAULT_PICK_FIRST: LazyLock<(Arc<DynLbPolicyBuilder>, DynLbConfig)> = LazyLock::new(|| {
    let builder = GLOBAL_LB_REGISTRY
        .get_policy(pick_first::POLICY_NAME)
        .expect("pick_first policy must be registered");
    let default_json = LbConfigJson::empty();
    let parsed_config = builder
        .parse_config(&default_json)
        .expect("pick first must parse an empty config");
    (builder, parsed_config)
});

pub type ParseResult = Result<ServiceConfig, String>;

/// An in-memory representation of a service config, provided to gRPC as a JSON
/// object.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    inner: serde_bindings::ServiceConfigSerde,
}

impl ServiceConfig {
    // Parses a service configuration from a JSON string.
    pub(crate) fn parse(config_json: &str) -> ParseResult {
        let config_serde: crate::client::service_config::serde_bindings::ServiceConfigSerde =
            serde_json::from_str(config_json)
                .map_err(|e| format!("failed to deserialize service config JSON: {e}"))?;
        config_serde.validate()?;
        Ok(Self {
            inner: config_serde,
        })
    }

    // Chooses load balancing configuration per gRPC specification rules.
    pub(crate) fn lb_config(&self) -> (Arc<DynLbPolicyBuilder>, DynLbConfig) {
        // Choose LbConfig if present.
        if let Some(selected) = self.inner.load_balancing_config.as_ref() {
            return (selected.builder.clone(), selected.config.clone());
        }

        // Fall back to legacy `loadBalancingPolicy` if present.
        if let Some(ref policy) = self.inner.load_balancing_policy
            && let Some(builder) = GLOBAL_LB_REGISTRY.get_policy(policy)
        {
            let empty_json = LbConfigJson::empty();
            if let Ok(parsed_config) = builder.parse_config(&empty_json) {
                return (builder, parsed_config);
            }
        }

        // Fall back to default policy.
        Self::default_lb_policy()
    }

    // Returns the default load balancing policy (`pick_first`).
    pub(crate) fn default_lb_policy() -> (Arc<DynLbPolicyBuilder>, DynLbConfig) {
        DEFAULT_PICK_FIRST.clone()
    }
}

#[cfg(test)]
mod test {
    use std::any::TypeId;
    use std::time::Duration;

    use serde_json::json;

    use super::duration::GrpcDuration;
    use super::serde_bindings::SerdeF32;
    use super::serde_bindings::SerdeU32;
    use super::*;
    use crate::client::load_balancing::ChannelController;
    use crate::client::load_balancing::LbPolicy;
    use crate::client::load_balancing::LbPolicyBuilder;
    use crate::client::load_balancing::LbPolicyOptions;
    use crate::client::load_balancing::WorkData;
    use crate::client::load_balancing::pick_first::PickFirstPolicy;
    use crate::client::load_balancing::round_robin::RoundRobinPolicy;
    use crate::client::name_resolution::ResolverUpdate;

    #[derive(Debug)]
    struct TestPolicyBuilder;
    impl LbPolicyBuilder for TestPolicyBuilder {
        type LbPolicy = TestPolicy;

        fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
            TestPolicy
        }

        fn name(&self) -> &'static str {
            "service_config_test_policy"
        }

        fn parse_config(
            &self,
            config: &LbConfigJson,
        ) -> Result<<Self::LbPolicy as crate::client::load_balancing::LbPolicy>::LbConfig, String>
        {
            let config: TestPolicyConfig = config.convert_to().map_err(|e| e.to_string())?;
            Ok(config)
        }
    }
    #[derive(Debug)]
    struct TestPolicy;
    impl LbPolicy for TestPolicy {
        type LbConfig = TestPolicyConfig;

        fn resolver_update(
            &mut self,
            update: ResolverUpdate,
            config: &Self::LbConfig,
            channel_controller: &mut dyn ChannelController,
        ) -> Result<(), String> {
            unimplemented!()
        }

        fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
            unimplemented!()
        }

        fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
            unimplemented!()
        }
    }
    #[derive(Debug, serde::Deserialize, Clone, Default)]
    struct TestPolicyConfig {
        #[serde(default, rename = "testField")]
        test_field: bool,
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_valid_service_config_parsing() {
        GLOBAL_LB_REGISTRY.add_builder(TestPolicyBuilder);

        let json_data = json!({
            "loadBalancingConfig": [
                { "service_config_test_policy": { "testField": true } },
                { "round_robin": {} }
            ],
            "methodConfig": [
                {
                    "name": [
                        { "service": "grpc.examples.echo.Echo", "method": "Echo" },
                        { "service": "grpc.examples.echo.Echo2" }
                    ],
                    "timeout": "1.5s",
                    "maxRequestMessageBytes": 1024,
                    "maxResponseMessageBytes": "2048"
                },
                {
                    "name": [
                        { "service": "grpc.examples.echo.EchoHedging" }
                    ],
                    "waitForReady": true
                }
            ],
            "retryThrottling": {
                "maxTokens": 100,
                "tokenRatio": 0.1
            },
            "healthCheckConfig": {
                "serviceName": "grpc.health.v1.Health"
            },
            "connectionScaling": {
                "maxConnectionsPerSubchannel": 20
            }
        });

        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();

        // Verify Load Balancing Config.
        let (builder, config) = sc.lb_config();
        assert_eq!(builder.name(), "service_config_test_policy");
        let pf_config = config.downcast_ref::<TestPolicyConfig>().unwrap().clone();
        assert!(pf_config.test_field);

        // Verify Method Config.
        let method_configs = sc.inner.method_config.unwrap();
        assert_eq!(method_configs.len(), 2);
        let mc = &method_configs[0];
        assert_eq!(mc.name.len(), 2);
        assert_eq!(mc.name[0].service, "grpc.examples.echo.Echo");
        assert_eq!(mc.name[0].method, "Echo");
        assert_eq!(mc.name[1].service, "grpc.examples.echo.Echo2");
        assert_eq!(mc.name[1].method, "");

        assert_eq!(
            mc.timeout,
            Some(GrpcDuration(Duration::new(1, 500_000_000)))
        );
        assert_eq!(mc.max_request_message_bytes, Some(SerdeU32(1024)));
        assert_eq!(mc.max_response_message_bytes, Some(SerdeU32(2048)));

        // Verify Retry Throttling.
        let rt = sc.inner.retry_throttling.unwrap();
        assert_eq!(rt.max_tokens, SerdeU32(100));
        assert_eq!(rt.token_ratio, SerdeF32(0.1));

        assert_eq!(
            sc.inner.health_check_config.as_ref().unwrap().service_name,
            Some("grpc.health.v1.Health".to_string())
        );
        assert_eq!(
            sc.inner
                .connection_scaling
                .as_ref()
                .unwrap()
                .max_connections_per_subchannel,
            SerdeU32(20)
        );
    }

    #[test]
    fn test_invalid_service_config_parsing() {
        // Bad JSON formatting.
        assert!(ServiceConfig::parse("{").is_err());

        // Invalid max tokens.
        let json_data = json!({
            "retryThrottling": {
                "maxTokens": 1001, // Invalid, max is 1000.
                "tokenRatio": 0.1
            }
        });
        assert!(ServiceConfig::parse(&json_data.to_string()).is_err());

        // Empty service name with non-empty method name.
        let json_data = json!({
            "methodConfig": [{
                "name": [{ "service": "", "method": "Echo" }]
            }]
        });
        assert!(ServiceConfig::parse(&json_data.to_string()).is_err());

        // Duplicate method_config name entry across items.
        let json_data = json!({
            "methodConfig": [
                {
                    "name": [{ "service": "foo", "method": "Bar" }]
                },
                {
                    "name": [{ "service": "foo", "method": "Bar" }]
                }
            ]
        });
        assert!(ServiceConfig::parse(&json_data.to_string()).is_err());

        // Non-empty loadBalancingConfig with no supported policy.
        let json_data = json!({
            "loadBalancingConfig": [
                { "unsupported_lb_policy": { "foo": "bar" } }
            ]
        });
        assert!(ServiceConfig::parse(&json_data.to_string()).is_err());
    }

    #[test]
    fn test_legacy_lb_policy_fallback() {
        let json_data = json!({
            "loadBalancingPolicy": "round_robin"
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        assert_eq!(
            sc.inner.load_balancing_policy,
            Some("round_robin".to_string())
        );
        assert!(sc.inner.load_balancing_config.is_none());
    }

    #[test]
    fn test_default_method_config_empty_name() {
        let json_data = json!({
            "methodConfig": [
                {
                    "name": [],
                    "timeout": "5s",
                    "waitForReady": true
                },
                {
                    "name": [{}],
                    "maxRequestMessageBytes": 4096
                }
            ]
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        let method_configs = sc.inner.method_config.unwrap();
        assert_eq!(method_configs.len(), 2);
        assert!(method_configs[0].name.is_empty());
        assert_eq!(
            method_configs[0].timeout,
            Some(GrpcDuration(Duration::from_secs(5)))
        );
        assert_eq!(method_configs[1].name.len(), 1);
        assert_eq!(method_configs[1].name[0].service, "");
        assert_eq!(method_configs[1].name[0].method, "");
    }

    #[test]
    fn test_service_level_vs_method_level_config() {
        let json_data = json!({
            "methodConfig": [
                {
                    "name": [{ "service": "grpc.examples.echo.Echo" }],
                },
                {
                    "name": [{ "service": "grpc.examples.echo.Echo", "method": "SpecialCall" }],
                    "timeout": "0.5s"
                }
            ]
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        let method_configs = sc.inner.method_config.unwrap();
        assert_eq!(method_configs[0].name[0].method, "");
        assert_eq!(method_configs[1].name[0].method, "SpecialCall");
        assert_eq!(
            method_configs[1].timeout,
            Some(GrpcDuration(Duration::from_millis(500)))
        );
    }

    #[test]
    fn test_minimal_lb_config_only() {
        let json_data = json!({
            "loadBalancingConfig": [
                { "round_robin": {} }
            ]
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        assert!(sc.inner.method_config.is_none());
        assert!(sc.inner.retry_throttling.is_none());
        assert!(sc.inner.health_check_config.is_none());
        assert!(sc.inner.connection_scaling.is_none());
        let (builder, config) = sc.lb_config();
        assert_eq!(builder.name(), "round_robin");
        assert!(config.type_id() == TypeId::of::<<RoundRobinPolicy as LbPolicy>::LbConfig>());
    }

    #[test]
    fn test_lb_config_resolution() {
        GLOBAL_LB_REGISTRY.add_builder(TestPolicyBuilder);

        // Explicit loadBalancingConfig selects first supported candidate
        let json_data = json!({
            "loadBalancingConfig": [
                { "unsupported_lb_policy": { "foo": "bar" } },
                { "service_config_test_policy": { "testField": true } },
                { "round_robin": {} }
            ]
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        let (builder, config) = sc.lb_config();
        assert_eq!(builder.name(), "service_config_test_policy");
        let pf_config = config.downcast_ref::<TestPolicyConfig>().unwrap().clone();
        assert!(pf_config.test_field);

        // Non-empty loadBalancingConfig with no supported policy errors on parse
        let json_data = json!({
            "loadBalancingConfig": [
                { "unsupported_lb_policy": { "foo": "bar" } }
            ]
        });
        assert!(ServiceConfig::parse(&json_data.to_string()).is_err());

        // Empty loadBalancingConfig array falls back to loadBalancingPolicy if present
        let json_data = json!({
            "loadBalancingConfig": [],
            "loadBalancingPolicy": "round_robin"
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        let (builder, config) = sc.lb_config();
        assert_eq!(builder.name(), "round_robin");
        assert!(config.type_id() == TypeId::of::<<RoundRobinPolicy as LbPolicy>::LbConfig>());

        // Empty loadBalancingConfig array with no loadBalancingPolicy falls back to default pick_first
        let json_data = json!({
            "loadBalancingConfig": []
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        let (builder, config) = sc.lb_config();
        assert_eq!(builder.name(), "pick_first");
        assert!(config.type_id() == TypeId::of::<<PickFirstPolicy as LbPolicy>::LbConfig>());

        // Legacy loadBalancingPolicy fallback when loadBalancingConfig is absent
        let json_data = json!({
            "loadBalancingPolicy": "round_robin"
        });
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        let (builder, config) = sc.lb_config();
        assert_eq!(builder.name(), "round_robin");
        assert!(config.type_id() == TypeId::of::<<RoundRobinPolicy as LbPolicy>::LbConfig>());

        // Neither loadBalancingConfig nor loadBalancingPolicy present -> default pick_first
        let json_data = json!({});
        let sc = ServiceConfig::parse(&json_data.to_string()).unwrap();
        let (builder, config) = sc.lb_config();
        assert_eq!(builder.name(), "pick_first");
        assert!(config.type_id() == TypeId::of::<<PickFirstPolicy as LbPolicy>::LbConfig>());
    }
}
