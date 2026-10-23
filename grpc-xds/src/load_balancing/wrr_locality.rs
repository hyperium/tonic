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

//! Implementation of the `xds_wrr_locality_experimental` Load Balancing policy.
//!
//! Used in xDS to distribute RPCs across localities using weighted round-robin
//! before delegating endpoint selection within the chosen locality to a
//! configured `childPolicy` (such as `round_robin`).
//!
//! Because xDS provides locality weights dynamically via EDS endpoint
//! attributes rather than static service config, this policy extracts those
//! per-locality weights from each resolver update and synthesizes a
//! `weighted_target_experimental` child policy configuration.
//!
//! Specifications:
//! - [gRFC A52: xDS Custom LB Policies](https://github.com/grpc/proposal/blob/master/A52-xds-custom-lb-policies.md)
//! - [gRFC A28: xDS Traffic Splitting and Routing](https://github.com/grpc/proposal/blob/master/A28-xds-traffic-splitting-and-routing.md#weighted-target-lb-policy)
//! - [gRFC A78: gRPC Metrics: WRR, Pick First, and xDS](https://github.com/grpc/proposal/blob/master/A78-grpc-metrics-wrr-pf-xds.md#optional-xds-locality-label)

use std::sync::Arc;

use grpc::__unstable::client::load_balancing::ChannelController;
use grpc::__unstable::client::load_balancing::LbConfigJson;
use grpc::__unstable::client::load_balancing::LbPolicy;
use grpc::__unstable::client::load_balancing::LbPolicyBuilder;
use grpc::__unstable::client::load_balancing::LbPolicyOptions;
use grpc::__unstable::client::load_balancing::LbState;
use grpc::__unstable::client::load_balancing::ParsedLbConfig;
use grpc::__unstable::client::load_balancing::PickOptions;
use grpc::__unstable::client::load_balancing::PickResult;
use grpc::__unstable::client::load_balancing::Picker;
use grpc::__unstable::client::load_balancing::WorkData;
use grpc::__unstable::client::load_balancing::WorkScheduler;
use grpc::__unstable::client::load_balancing::child_manager::Child;
use grpc::__unstable::client::load_balancing::registry::GLOBAL_LB_REGISTRY;
use grpc::__unstable::client::name_resolution::Endpoint;
use grpc::__unstable::client::name_resolution::ResolverUpdate;
use grpc::__unstable::rt::GrpcRuntime;
use grpc::StatusCodeError;
use grpc::StatusError;
use grpc::client::ConnectivityState;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::resource::Locality;

static POLICY_NAME: &str = "xds_wrr_locality_experimental";
static CHILD_POLICY_NAME: &str = "weighted_target_experimental";

// Endpoint attribute carrying the xDS locality load-balancing weight.
#[derive(Debug, PartialEq, Eq)]
struct LocalityWeight(u32);

// Validated configuration for the WRR Locality LB policy.
#[derive(Debug, Deserialize)]
struct WrrLocalityConfig {
    // Prototype LB policy configuration for picking endpoints within a
    // locality. Kept as raw JSON so it can be replicated into each locality
    // entry when constructing the `weighted_target_experimental` config on
    // resolver updates.
    #[serde(rename = "childPolicy")]
    child_policy: Vec<Value>,
}

#[derive(Debug)]
struct WrrLocalityBuilder;

#[derive(Debug)]
struct WrrLocalityPolicy {
    child: Option<(Child, ParsedLbConfig)>,
    runtime: GrpcRuntime,
    work_scheduler: Arc<dyn WorkScheduler>,
    #[cfg(test)]
    child_policy_name: &'static str,
}

// Picker that fails every pick with UNAVAILABLE when no weighted localities
// can be configured.
#[derive(Debug)]
struct FailingPicker {
    error: String,
}

impl WrrLocalityBuilder {
    #[cfg(test)]
    fn build_with_child_policy_name(
        &self,
        options: LbPolicyOptions,
        child_policy_name: &'static str,
    ) -> WrrLocalityPolicy {
        WrrLocalityPolicy {
            child_policy_name,
            ..self.build(options)
        }
    }
}

impl LbPolicyBuilder for WrrLocalityBuilder {
    type LbPolicy = WrrLocalityPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        WrrLocalityPolicy {
            child: None,
            runtime: options.runtime,
            work_scheduler: options.work_scheduler,
            #[cfg(test)]
            child_policy_name: CHILD_POLICY_NAME,
        }
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }

    fn parse_config(&self, config: &LbConfigJson) -> Result<WrrLocalityConfig, String> {
        let parsed: WrrLocalityConfig = config
            .convert_to()
            .map_err(|e| format!("{POLICY_NAME}: invalid config: {e}"))?;

        serde_json::from_value::<ParsedLbConfig>(Value::Array(parsed.child_policy.clone()))
            .map_err(|e| format!("{POLICY_NAME}: invalid childPolicy: {e}"))?;

        Ok(parsed)
    }
}

impl WrrLocalityPolicy {
    fn child_policy_name(&self) -> &'static str {
        #[cfg(not(test))]
        {
            CHILD_POLICY_NAME
        }
        #[cfg(test)]
        {
            self.child_policy_name
        }
    }

    // Drops any active child policy, transitions the channel to
    // TRANSIENT_FAILURE with a FailingPicker, and returns the error.
    fn fail_update<T>(
        &mut self,
        error: String,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<T, String> {
        self.child = None;
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::TransientFailure,
            picker: Arc::new(FailingPicker {
                error: error.clone(),
            }),
        });
        Err(error)
    }

    // Synthesizes and parses the child LB policy configuration from
    // per-locality endpoint weights. Per gRFC A52, if a locality appears with
    // conflicting weights across endpoints, the first weight encountered is
    // used. Fails the update if no localities with positive weight are present
    // or if the generated child config fails to parse.
    fn parse_child_config(
        &mut self,
        endpoints: &[Endpoint],
        child_policy: &[Value],
        channel_controller: &mut dyn ChannelController,
    ) -> Result<ParsedLbConfig, String> {
        let mut targets = serde_json::Map::new();
        for (locality, weight) in endpoints.iter().filter_map(|endpoint| {
            let locality = endpoint.attributes.get::<Locality>()?;
            let &LocalityWeight(weight) = endpoint.attributes.get::<LocalityWeight>()?;
            (weight > 0).then_some((locality, weight))
        }) {
            let locality_key = format!(
                "{{region=\"{}\", zone=\"{}\", sub_zone=\"{}\"}}",
                locality.region, locality.zone, locality.sub_zone
            );
            targets.entry(locality_key).or_insert_with(|| {
                json!({
                    "weight": weight,
                    "childPolicy": child_policy,
                })
            });
        }

        if targets.is_empty() {
            return self.fail_update(
                format!("{POLICY_NAME}: no localities with positive weight in resolver update"),
                channel_controller,
            );
        }

        let child_policy_json = json!([{
            self.child_policy_name(): {
                "targets": targets,
            }
        }]);
        match ParsedLbConfig::parse(&child_policy_json.to_string()) {
            Ok(parsed) => Ok(parsed),
            Err(err) => self.fail_update(
                format!("{POLICY_NAME}: failed to parse generated child config: {err}"),
                channel_controller,
            ),
        }
    }

    // Lazily initializes the child policy on first update and stores the latest
    // parsed child configuration.
    fn get_or_insert_child(
        &mut self,
        child_config: ParsedLbConfig,
    ) -> &mut (Child, ParsedLbConfig) {
        if let Some((_, last_config)) = &mut self.child {
            *last_config = child_config;
        } else {
            let child = Child::new(
                &*child_config.builder,
                LbPolicyOptions {
                    work_scheduler: self.work_scheduler.clone(),
                    runtime: self.runtime.clone(),
                },
            );
            self.child = Some((child, child_config));
        }
        self.child.as_mut().unwrap()
    }

    // Use whenever forwarding a resolver update to an active child so that any
    // picker intercepted by `Child` is published to the channel.
    fn update_child(
        child: &mut Child,
        config: &ParsedLbConfig,
        update: ResolverUpdate,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let result = child.resolver_update(update, &config.config, channel_controller);
        if child.take_updated() {
            channel_controller.update_picker(child.state().clone());
        }
        result.map_err(|e| format!("{POLICY_NAME}: failed to update child policy: {e}"))
    }
}

impl LbPolicy for WrrLocalityPolicy {
    type LbConfig = WrrLocalityConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let endpoints = match &update.endpoints {
            Ok(endpoints) => endpoints,
            Err(err) => {
                let Some((child, last_config)) = &mut self.child else {
                    return self.fail_update(
                        format!("{POLICY_NAME}: resolver error: {err}"),
                        channel_controller,
                    );
                };

                return Self::update_child(child, last_config, update, channel_controller);
            }
        };

        let child_config =
            self.parse_child_config(endpoints, &config.child_policy, channel_controller)?;
        let (child, last_config) = self.get_or_insert_child(child_config);
        Self::update_child(child, last_config, update, channel_controller)
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        let Some(data) = data else {
            debug_assert!(false, "{POLICY_NAME}: work called with None value");
            return;
        };
        let Some((child, _)) = &mut self.child else {
            return;
        };
        if child.try_work(data, channel_controller).is_ok() && child.take_updated() {
            channel_controller.update_picker(child.state().clone());
        }
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        let Some((child, _)) = &mut self.child else {
            return;
        };
        child.exit_idle(channel_controller);
        if child.take_updated() {
            channel_controller.update_picker(child.state().clone());
        }
    }
}

impl Picker for FailingPicker {
    fn pick(&self, _options: PickOptions<'_>) -> PickResult {
        PickResult::Fail(StatusError::new(
            StatusCodeError::Unavailable,
            self.error.clone(),
        ))
    }
}

/// Register WRR locality as an LbPolicy.
pub(crate) fn reg() {
    GLOBAL_LB_REGISTRY.add_builder(WrrLocalityBuilder);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use grpc::__unstable::client::load_balancing::round_robin::POLICY_NAME as RR_POLICY_NAME;
    use grpc::__unstable::client::load_balancing::subchannel::Subchannel;
    use grpc::__unstable::client::load_balancing::subchannel::SubchannelState;
    use grpc::client::CallOptions;
    use grpc::client::RequestHeaders;
    use grpc::core::Address;
    use serde::Serialize;

    use super::*;

    #[test]
    fn parse_config() {
        let builder = WrrLocalityBuilder;
        assert_eq!(builder.name(), "xds_wrr_locality_experimental");

        let parse =
            |json: Value| builder.parse_config(&LbConfigJson::new(&json.to_string()).unwrap());

        let valid = parse(json!({
            "childPolicy": [{ RR_POLICY_NAME: {} }]
        }))
        .expect("valid config should parse");
        assert_eq!(valid.child_policy, vec![json!({ RR_POLICY_NAME: {} })]);

        let fallback = parse(json!({
            "childPolicy": [
                { "unregistered_policy_name": {} },
                { RR_POLICY_NAME: {} }
            ]
        }))
        .expect("fallback config with a supported policy should parse");
        assert_eq!(fallback.child_policy.len(), 2);

        let err = |json| parse(json).expect_err("invalid config should fail to parse");
        assert!(err(json!({})).contains("childPolicy"));
        assert!(err(json!({ "childPolicy": null })).contains("invalid config"));
        assert!(err(json!({ "childPolicy": {} })).contains("invalid config"));
        assert!(err(json!({ "childPolicy": [] })).contains("must not be empty"));
        assert!(err(json!({ "childPolicy": [{ "": {} }] })).contains("No supported"));
        assert!(
            err(json!({ "childPolicy": [{ "unregistered_policy_name": {} }] }))
                .contains("No supported")
        );
        assert!(
            err(json!({
                "childPolicy": [{ "pick_first": { "shuffleAddressList": "not_a_bool" } }]
            }))
            .contains("invalid childPolicy")
        );
    }

    #[test]
    fn resolver_update_synthesizes_weighted_targets_and_forwards_update() {
        let mut t = Harness::new();
        let endpoints = vec![
            locality_endpoint("region-1", "zone-1", "subzone-1", 20),
            locality_endpoint("region-2", "zone-2", "subzone-2", 80),
        ];
        let mut update = ResolverUpdate::default();
        update.attributes = update.attributes.add("custom_attr");
        update.endpoints = Ok(endpoints.clone());
        update.resolution_note = Some("eds_note".to_string());

        t.update(update, MockBehavior::Ready)
            .expect("resolver update should succeed");

        let state = t.take_state();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let picked = assert_picks(&*state.picker);
        assert_eq!(
            picked.targets,
            BTreeMap::from([
                (
                    "{region=\"region-1\", zone=\"zone-1\", sub_zone=\"subzone-1\"}".to_string(),
                    20,
                ),
                (
                    "{region=\"region-2\", zone=\"zone-2\", sub_zone=\"subzone-2\"}".to_string(),
                    80,
                ),
            ])
        );
        assert_eq!(picked.update.endpoints, Ok(endpoints));
        assert_eq!(picked.update.attributes.get(), Some(&"custom_attr"));
        assert_eq!(picked.update.resolution_note.as_deref(), Some("eds_note"));
    }

    #[test]
    fn conflicting_weights_for_same_locality_uses_first_weight() {
        let mut t = Harness::new();
        let endpoints = vec![
            locality_endpoint("r1", "z1", "sz1", 35),
            locality_endpoint("r1", "z1", "sz1", 35),
            locality_endpoint("r1", "z1", "sz1", 90),
        ];

        assert_eq!(
            t.parse_child_targets(&endpoints),
            BTreeMap::from([(
                "{region=\"r1\", zone=\"z1\", sub_zone=\"sz1\"}".to_string(),
                35,
            )])
        );
    }

    #[test]
    fn endpoints_without_locality_or_positive_weight_are_skipped() {
        let mut t = Harness::new();
        let mut missing_weight = Endpoint::default();
        missing_weight.attributes = missing_weight.attributes.add(Locality {
            region: "r1".to_string(),
            zone: "z1".to_string(),
            sub_zone: "sz1".to_string(),
        });
        let mut missing_locality = Endpoint::default();
        missing_locality.attributes = missing_locality.attributes.add(LocalityWeight(10));

        let endpoints = vec![
            missing_weight,
            missing_locality,
            locality_endpoint("r2", "z2", "sz2", 0),
            locality_endpoint("r3", "z3", "sz3", 50),
        ];

        assert_eq!(
            t.parse_child_targets(&endpoints),
            BTreeMap::from([(
                "{region=\"r3\", zone=\"z3\", sub_zone=\"sz3\"}".to_string(),
                50,
            )])
        );
    }

    #[test]
    fn dynamic_locality_updates_refresh_targets() {
        let mut t = Harness::new();

        let mut first = ResolverUpdate::default();
        first.endpoints = Ok(vec![
            locality_endpoint("r1", "z1", "sz1", 10),
            locality_endpoint("r2", "z2", "sz2", 20),
        ]);
        t.update(first, MockBehavior::Ready)
            .expect("initial update should succeed");
        let picked = assert_picks(&*t.take_state().picker);
        assert_eq!(picked.targets.len(), 2);

        let mut second = ResolverUpdate::default();
        second.endpoints = Ok(vec![locality_endpoint("r2", "z2", "sz2", 75)]);
        t.update(second, MockBehavior::Ready)
            .expect("second update should succeed");
        let picked = assert_picks(&*t.take_state().picker);
        assert_eq!(
            picked.targets,
            BTreeMap::from([(
                "{region=\"r2\", zone=\"z2\", sub_zone=\"sz2\"}".to_string(),
                75,
            )])
        );
    }

    #[test]
    fn empty_or_unweighted_endpoints_transitions_to_transient_failure() {
        let mut t = Harness::new();

        // Establish an active READY child first to verify it is replaced.
        let mut initial = ResolverUpdate::default();
        initial.endpoints = Ok(vec![locality_endpoint("r1", "z1", "sz1", 10)]);
        t.update(initial, MockBehavior::Ready)
            .expect("initial update should succeed");
        assert_eq!(t.take_state().connectivity_state, ConnectivityState::Ready);

        // Update with an endpoint missing locality weight.
        let mut unweighted = Endpoint::default();
        unweighted.attributes = unweighted.attributes.add(Locality {
            region: "r1".to_string(),
            zone: "z1".to_string(),
            sub_zone: "sz1".to_string(),
        });
        let mut update = ResolverUpdate::default();
        update.endpoints = Ok(vec![unweighted]);

        let err = t
            .update(update, MockBehavior::Ready)
            .expect_err("update without weighted localities should fail");
        assert!(err.contains("no localities with positive weight"));

        let state = t.take_state();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        assert_fails_unavailable(&*state.picker, "no localities with positive weight");
    }

    #[test]
    fn resolver_error_without_child_transitions_to_transient_failure() {
        let mut t = Harness::new();
        let mut update = ResolverUpdate::default();
        update.endpoints = Err("eds stream closed".to_string());

        let err = t
            .update(update, MockBehavior::Ready)
            .expect_err("resolver error without child should fail update");
        assert!(err.contains("eds stream closed"));

        let state = t.take_state();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        assert_fails_unavailable(&*state.picker, "eds stream closed");
    }

    #[test]
    fn resolver_error_with_existing_child_forwards_to_child() {
        let mut t = Harness::new();
        let mut initial = ResolverUpdate::default();
        initial.endpoints = Ok(vec![locality_endpoint("r1", "z1", "sz1", 40)]);
        t.update(initial, MockBehavior::Ready)
            .expect("initial update should succeed");
        assert_eq!(t.take_state().connectivity_state, ConnectivityState::Ready);

        let mut err_update = ResolverUpdate::default();
        err_update.endpoints = Err("eds stream closed".to_string());
        t.update(err_update, MockBehavior::Ready)
            .expect("resolver error should be forwarded to existing child");

        let state = t.take_state();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let picked = assert_picks(&*state.picker);
        assert_eq!(
            picked.targets,
            BTreeMap::from([(
                "{region=\"r1\", zone=\"z1\", sub_zone=\"sz1\"}".to_string(),
                40,
            )])
        );
        assert_eq!(
            picked.update.endpoints,
            Err("eds stream closed".to_string())
        );
    }

    #[test]
    fn generated_child_config_parse_error_transitions_to_transient_failure() {
        let mut t = Harness::with_child_policy_name("unregistered_child_policy");
        let mut update = ResolverUpdate::default();
        update.endpoints = Ok(vec![locality_endpoint("r1", "z1", "sz1", 10)]);

        let err = t
            .update(update, MockBehavior::Ready)
            .expect_err("unregistered child policy should fail update");
        assert!(err.contains("failed to parse generated child config"));

        let state = t.take_state();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        assert_fails_unavailable(&*state.picker, "failed to parse generated child config");
    }

    #[test]
    fn child_error_still_publishes_picker() {
        let mut t = Harness::new();
        let mut update = ResolverUpdate::default();
        update.endpoints = Ok(vec![locality_endpoint("r1", "z1", "sz1", 10)]);

        let err = t
            .update(update, MockBehavior::PublishReadyAndFail)
            .expect_err("child failure should return error");
        assert_eq!(
            err,
            "xds_wrr_locality_experimental: failed to update child policy: mock child failure"
        );

        let state = t.take_state();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let picked = assert_picks(&*state.picker);
        assert_eq!(picked.targets.len(), 1);
    }

    #[test]
    fn work_and_exit_idle_delegate_to_child_and_republish_on_update() {
        let mut t = Harness::new();
        let mut update = ResolverUpdate::default();
        update.endpoints = Ok(vec![locality_endpoint("r1", "z1", "sz1", 10)]);

        // Child starts IDLE.
        t.update(update, MockBehavior::Idle)
            .expect("update should succeed");
        assert_eq!(t.take_state().connectivity_state, ConnectivityState::Idle);

        // `exit_idle` moves child to CONNECTING and schedules work.
        t.policy.exit_idle(&mut t.controller);
        assert_eq!(
            t.take_state().connectivity_state,
            ConnectivityState::Connecting
        );

        // Running scheduled work moves child to READY.
        t.run_work();
        let state = t.take_state();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        assert_eq!(assert_picks(&*state.picker).targets.len(), 1);

        // Calling `exit_idle` when already READY does not republish a picker.
        t.policy.exit_idle(&mut t.controller);
        assert!(
            t.controller.states.is_empty(),
            "policy should not republish when child did not update"
        );
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "xds_wrr_locality_experimental: work called with None value")
    )]
    fn work_with_none_panics_in_debug() {
        let mut t = Harness::new();
        t.policy.work(None, &mut t.controller);
    }

    struct Harness {
        policy: WrrLocalityPolicy,
        controller: RecordingChannelController,
        scheduler: Arc<RecordingWorkScheduler>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_child_policy_name(MOCK_CHILD_POLICY_NAME)
        }

        fn with_child_policy_name(child_policy_name: &'static str) -> Self {
            GLOBAL_LB_REGISTRY.add_builder(MockChildBuilder);
            GLOBAL_LB_REGISTRY.add_builder(MockLeafBuilder);
            let scheduler = Arc::new(RecordingWorkScheduler::default());
            let policy = WrrLocalityBuilder.build_with_child_policy_name(
                LbPolicyOptions {
                    work_scheduler: scheduler.clone(),
                    runtime: grpc::__unstable::rt::default_runtime(),
                },
                child_policy_name,
            );
            Self {
                policy,
                controller: RecordingChannelController::default(),
                scheduler,
            }
        }

        fn update(&mut self, update: ResolverUpdate, behavior: MockBehavior) -> Result<(), String> {
            let json = json!({
                "childPolicy": [{
                    MOCK_LEAF_POLICY_NAME: { "behavior": behavior }
                }]
            });
            let config = WrrLocalityBuilder
                .parse_config(&LbConfigJson::new(&json.to_string()).unwrap())
                .expect("test config should parse");
            self.policy
                .resolver_update(update, &config, &mut self.controller)
        }

        fn parse_child_targets(&mut self, endpoints: &[Endpoint]) -> BTreeMap<String, u32> {
            let child_policy = vec![json!({ RR_POLICY_NAME: {} })];
            let parsed = self
                .policy
                .parse_child_config(endpoints, &child_policy, &mut self.controller)
                .expect("child config should parse");
            let child_cfg = parsed
                .config
                .downcast_ref::<MockChildConfig>()
                .expect("child config should be MockChildConfig");
            assert!(
                child_cfg
                    .targets
                    .values()
                    .all(|target| target.child_policy.builder.name() == RR_POLICY_NAME)
            );
            child_cfg
                .targets
                .iter()
                .map(|(name, target)| (name.clone(), target.weight))
                .collect()
        }

        fn take_state(&mut self) -> LbState {
            let mut states = std::mem::take(&mut self.controller.states);
            assert_eq!(states.len(), 1, "policy should publish exactly one state");
            states.pop().unwrap()
        }

        fn run_work(&mut self) {
            let mut work = std::mem::take(&mut *self.scheduler.0.lock().unwrap());
            assert_eq!(work.len(), 1, "exactly one work item should be pending");
            self.policy.work(work.pop().unwrap(), &mut self.controller);
        }
    }

    fn locality_endpoint(region: &str, zone: &str, sub_zone: &str, weight: u32) -> Endpoint {
        let mut endpoint = Endpoint::default();
        endpoint.attributes = endpoint
            .attributes
            .add(Locality {
                region: region.to_string(),
                zone: zone.to_string(),
                sub_zone: sub_zone.to_string(),
            })
            .add(LocalityWeight(weight));
        endpoint
    }

    fn assert_picks(picker: &dyn Picker) -> PickedState {
        let headers = RequestHeaders::new();
        let slot: PickedSlot = Arc::default();
        let mut call_options = CallOptions::new();
        call_options.set_option(slot.clone());
        let result = picker.pick(PickOptions::new(&headers, &call_options));
        assert!(
            matches!(result, PickResult::Queue),
            "pick should queue, got {result:?}"
        );
        slot.lock()
            .unwrap()
            .take()
            .expect("pick should reach mock child")
    }

    fn assert_fails_unavailable(picker: &dyn Picker, expected_substring: &str) {
        let headers = RequestHeaders::new();
        let call_options = CallOptions::new();
        match picker.pick(PickOptions::new(&headers, &call_options)) {
            PickResult::Fail(err) => {
                assert_eq!(err.code(), StatusCodeError::Unavailable);
                assert!(
                    err.message().contains(expected_substring),
                    "error '{}' should contain '{expected_substring}'",
                    err.message()
                );
            }
            other => panic!("expected Fail(Unavailable), got {other:?}"),
        }
    }

    const MOCK_CHILD_POLICY_NAME: &str = "test_wrr_locality_mock_child";
    const MOCK_LEAF_POLICY_NAME: &str = "test_wrr_locality_mock_leaf";

    #[derive(Clone, Copy, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    enum MockBehavior {
        Ready,
        Idle,
        PublishReadyAndFail,
    }

    #[derive(Debug, Deserialize)]
    struct MockLeafConfig {
        behavior: MockBehavior,
    }

    #[derive(Debug)]
    struct MockLeafPolicy;

    impl LbPolicy for MockLeafPolicy {
        type LbConfig = MockLeafConfig;

        fn resolver_update(
            &mut self,
            _update: ResolverUpdate,
            _config: &Self::LbConfig,
            _channel_controller: &mut dyn ChannelController,
        ) -> Result<(), String> {
            unreachable!("leaf policy is only parsed, not driven directly by wrr_locality")
        }

        fn work(
            &mut self,
            _data: Option<WorkData>,
            _channel_controller: &mut dyn ChannelController,
        ) {
            unreachable!()
        }

        fn exit_idle(&mut self, _channel_controller: &mut dyn ChannelController) {
            unreachable!()
        }
    }

    #[derive(Debug)]
    struct MockLeafBuilder;

    impl LbPolicyBuilder for MockLeafBuilder {
        type LbPolicy = MockLeafPolicy;

        fn build(&self, _options: LbPolicyOptions) -> Self::LbPolicy {
            MockLeafPolicy
        }

        fn name(&self) -> &'static str {
            MOCK_LEAF_POLICY_NAME
        }

        fn parse_config(&self, config: &LbConfigJson) -> Result<MockLeafConfig, String> {
            config.convert_to().map_err(|e| e.to_string())
        }
    }

    #[derive(Debug, Deserialize)]
    struct MockTargetConfig {
        weight: u32,
        #[serde(rename = "childPolicy")]
        child_policy: ParsedLbConfig,
    }

    #[derive(Debug, Deserialize)]
    struct MockChildConfig {
        targets: BTreeMap<String, MockTargetConfig>,
    }

    #[derive(Clone, Debug)]
    struct PickedState {
        targets: BTreeMap<String, u32>,
        update: ResolverUpdate,
    }

    type PickedSlot = Arc<Mutex<Option<PickedState>>>;

    #[derive(Debug)]
    struct MockPicker(PickedState);

    impl Picker for MockPicker {
        fn pick(&self, options: PickOptions<'_>) -> PickResult {
            let slot = options
                .call_options
                .get_option::<PickedSlot>()
                .expect("pick should include PickedSlot option");
            *slot.lock().unwrap() = Some(self.0.clone());
            PickResult::Queue
        }
    }

    #[derive(Debug)]
    struct MockChildBuilder;

    impl LbPolicyBuilder for MockChildBuilder {
        type LbPolicy = MockChildPolicy;

        fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
            MockChildPolicy {
                work_scheduler: options.work_scheduler,
                state: None,
                picked: None,
            }
        }

        fn name(&self) -> &'static str {
            MOCK_CHILD_POLICY_NAME
        }

        fn parse_config(&self, config: &LbConfigJson) -> Result<MockChildConfig, String> {
            config.convert_to().map_err(|e| e.to_string())
        }
    }

    #[derive(Debug)]
    struct MockChildPolicy {
        work_scheduler: Arc<dyn WorkScheduler>,
        state: Option<ConnectivityState>,
        picked: Option<PickedState>,
    }

    impl MockChildPolicy {
        fn publish(&mut self, state: ConnectivityState, controller: &mut dyn ChannelController) {
            self.state = Some(state);
            controller.update_picker(LbState {
                connectivity_state: state,
                picker: Arc::new(MockPicker(
                    self.picked.clone().expect("picked state should be set"),
                )),
            });
        }
    }

    impl LbPolicy for MockChildPolicy {
        type LbConfig = MockChildConfig;

        fn resolver_update(
            &mut self,
            update: ResolverUpdate,
            config: &Self::LbConfig,
            channel_controller: &mut dyn ChannelController,
        ) -> Result<(), String> {
            let mut behavior = MockBehavior::Ready;
            let targets = config
                .targets
                .iter()
                .map(|(name, target)| {
                    let leaf_cfg = target
                        .child_policy
                        .config
                        .downcast_ref::<MockLeafConfig>()
                        .expect("child policy should be MockLeafConfig");
                    behavior = leaf_cfg.behavior;
                    (name.clone(), target.weight)
                })
                .collect();

            self.picked = Some(PickedState { targets, update });

            match behavior {
                MockBehavior::Ready => {
                    self.publish(ConnectivityState::Ready, channel_controller);
                    Ok(())
                }
                MockBehavior::Idle => {
                    self.publish(ConnectivityState::Idle, channel_controller);
                    Ok(())
                }
                MockBehavior::PublishReadyAndFail => {
                    self.publish(ConnectivityState::Ready, channel_controller);
                    Err("mock child failure".to_string())
                }
            }
        }

        fn work(
            &mut self,
            _data: Option<WorkData>,
            channel_controller: &mut dyn ChannelController,
        ) {
            if self.state == Some(ConnectivityState::Connecting) {
                self.publish(ConnectivityState::Ready, channel_controller);
            }
        }

        fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
            if self.state == Some(ConnectivityState::Idle) {
                self.publish(ConnectivityState::Connecting, channel_controller);
                self.work_scheduler.schedule_work(None);
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingWorkScheduler(Mutex<Vec<Option<WorkData>>>);

    impl WorkScheduler for RecordingWorkScheduler {
        fn schedule_work(&self, data: Option<WorkData>) {
            self.0.lock().unwrap().push(data);
        }
    }

    #[derive(Default)]
    struct RecordingChannelController {
        states: Vec<LbState>,
    }

    impl ChannelController for RecordingChannelController {
        fn new_subchannel(
            &mut self,
            _address: &Address,
            _work_scheduler: Arc<dyn WorkScheduler>,
        ) -> (Arc<dyn Subchannel>, SubchannelState) {
            unimplemented!()
        }

        fn update_picker(&mut self, update: LbState) {
            self.states.push(update);
        }

        fn request_resolution(&mut self) {
            unimplemented!()
        }
    }
}
