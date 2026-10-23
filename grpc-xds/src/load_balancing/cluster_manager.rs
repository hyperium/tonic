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

//! Implementation of the `xds_cluster_manager_experimental` Load Balancing policy.
//!
//! The Cluster Manager LB policy ([gRFC A31](https://github.com/grpc/proposal/blob/master/A31-xds-timeout-support-and-config-selector.md))
//! manages a list of child LB policies, each corresponding to an xDS cluster.
//! This policy selects a child policy (and therefore cluster) to route each RPC
//! to.
//!
//! Clusters removed from the configuration are shut down immediately.
//!
//! TODO(https://github.com/grpc/grpc-rust/issues/2890): gRFC A31 does not
//! specify retention behaviour for removed clusters, and existing
//! implementations in other languages disagree. Revisit once there is
//! cross-language agreement on the intended behaviour or we implement
//! subchannel caching, which serves the same purpose.

use std::collections::HashMap;
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
use grpc::__unstable::client::load_balancing::child_manager::ChildManager;
use grpc::__unstable::client::load_balancing::child_manager::ChildUpdate;
use grpc::__unstable::client::load_balancing::registry::GLOBAL_LB_REGISTRY;
use grpc::__unstable::client::name_resolution::ResolverUpdate;
use grpc::StatusCodeError;
use grpc::StatusError;
use serde::Deserialize;

static POLICY_NAME: &str = "xds_cluster_manager_experimental";

// Target cluster option for an RPC, keyed by its `TypeId` in `CallOptions`.
#[derive(Debug, Clone)]
struct XdsCluster(Arc<str>);

// Validated configuration for `xds_cluster_manager_experimental`.
// Maps a cluster name to a load balancing configuration for that cluster.
#[derive(Clone, Debug, Deserialize)]
struct ClusterManagerConfig {
    children: HashMap<String, ClusterChildConfig>,
}

#[derive(Clone, Debug, Deserialize)]
struct ClusterChildConfig {
    #[serde(rename = "childPolicy")]
    child_policy: ParsedLbConfig,
}

#[derive(Debug, Default)]
struct ClusterManagerBuilder;

impl LbPolicyBuilder for ClusterManagerBuilder {
    type LbPolicy = ClusterManagerPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        ClusterManagerPolicy::new(options)
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }

    fn parse_config(&self, config: &LbConfigJson) -> Result<ClusterManagerConfig, String> {
        let parsed: ClusterManagerConfig = config
            .convert_to()
            .map_err(|e| format!("failed to deserialize xds_cluster_manager config: {e}"))?;

        if parsed.children.is_empty() {
            return Err(
                "failed to parse xds_cluster_manager config: 'children' must be non-empty"
                    .to_string(),
            );
        }

        Ok(parsed)
    }
}

// An instance of the Cluster Manager Load Balancing policy.
#[derive(Debug)]
struct ClusterManagerPolicy {
    child_manager: ChildManager<String>,
}

impl ClusterManagerPolicy {
    fn new(options: LbPolicyOptions) -> Self {
        Self {
            child_manager: ChildManager::new(options.runtime, options.work_scheduler),
        }
    }

    // Publishes a picker covering every configured cluster, together with the
    // aggregate connectivity state of the children.
    fn update_picker(&mut self, channel_controller: &mut dyn ChannelController) {
        let connectivity_state = self.child_manager.aggregate_states();
        let children = self
            .child_manager
            .children()
            .map(|(id, child)| (id.clone(), child.state().picker.clone()))
            .collect::<HashMap<_, _>>();

        channel_controller.update_picker(LbState {
            connectivity_state,
            picker: Arc::new(ClusterPicker { children }),
        });
    }
}

impl LbPolicy for ClusterManagerPolicy {
    type LbConfig = ClusterManagerConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        // `ChildManager::update` removes any existing child that is absent from
        // this list, so clusters dropped from the config are removed here.
        let child_updates = config
            .children
            .iter()
            .map(|(cluster, child_cfg)| ChildUpdate {
                child_identifier: cluster.clone(),
                child_policy_builder: child_cfg.child_policy.builder.clone(),
                child_update: Some((update.clone(), &child_cfg.child_policy.config)),
            });

        let result = self.child_manager.update(child_updates, channel_controller);
        self.update_picker(channel_controller);
        result.map_err(|e| format!("failed to update xds_cluster_manager children: {e}"))
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        // Forward data unconditionally: `ChildManager::work` debug_asserts on a
        // `None` payload, and suppressing that here would hide the violation.
        self.child_manager.work(data, channel_controller);
        if self.child_manager.child_updated() {
            self.update_picker(channel_controller);
        }
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.child_manager.exit_idle(channel_controller);
        if self.child_manager.child_updated() {
            self.update_picker(channel_controller);
        }
    }
}

// Picker that delegates to the child picker corresponding to the cluster
// call option.
#[derive(Debug)]
struct ClusterPicker {
    children: HashMap<String, Arc<dyn Picker>>,
}

impl Picker for ClusterPicker {
    fn pick(&self, options: PickOptions<'_>) -> PickResult {
        options
            .call_options
            .get_option::<XdsCluster>()
            .ok_or_else(|| {
                debug_assert!(false, "cluster manager: cluster attribute not present");
                StatusError::new(
                    StatusCodeError::Internal,
                    "cluster manager: cluster attribute not present",
                )
            })
            .and_then(|cluster| {
                self.children.get(&*cluster.0).ok_or_else(|| {
                    debug_assert!(false, "cluster manager: unknown cluster '{}'", cluster.0);
                    StatusError::new(
                        StatusCodeError::Internal,
                        format!("cluster manager: unknown cluster '{}'", cluster.0),
                    )
                })
            })
            .map_or_else(PickResult::Drop, |picker| picker.pick(options))
    }
}

/// Register cluster manager as a LbPolicy.
pub(crate) fn reg() {
    GLOBAL_LB_REGISTRY.add_builder(ClusterManagerBuilder {});
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use grpc::__unstable::client::load_balancing::GLOBAL_LB_REGISTRY;
    use grpc::__unstable::client::load_balancing::WorkScheduler;
    use grpc::__unstable::client::load_balancing::round_robin::POLICY_NAME as RR_POLICY_NAME;
    use grpc::__unstable::client::load_balancing::subchannel::Subchannel;
    use grpc::__unstable::client::load_balancing::subchannel::SubchannelState;
    use grpc::__unstable::client::name_resolution::Endpoint;
    use grpc::__unstable::rt::default_runtime;
    use grpc::client::CallOptions;
    use grpc::client::ConnectivityState;
    use grpc::client::RequestHeaders;
    use grpc::core::Address;

    use super::*;

    #[test]
    fn parse_config() {
        let builder = ClusterManagerBuilder;
        assert_eq!(builder.name(), "xds_cluster_manager_experimental");

        let valid_json = serde_json::json!({
            "children": {
                "cluster_a": { "childPolicy": [{ RR_POLICY_NAME: {} }] },
                "cluster_b": { "childPolicy": [{ RR_POLICY_NAME: {} }] }
            }
        });
        let config = builder
            .parse_config(&LbConfigJson::new(&valid_json.to_string()).unwrap())
            .expect("parse valid config");
        assert_eq!(config.children.len(), 2);
        assert!(config.children.contains_key("cluster_a"));
        assert!(config.children.contains_key("cluster_b"));

        let empty_children = serde_json::json!({ "children": {} });
        let err = builder
            .parse_config(&LbConfigJson::new(&empty_children.to_string()).unwrap())
            .unwrap_err();
        assert!(err.contains("children"));
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "unknown cluster"))]
    fn cluster_picker_routing() {
        let mut children: HashMap<String, Arc<dyn Picker>> = HashMap::new();
        children.insert(
            "cluster_one".to_string(),
            Arc::new(TaggedPicker {
                tag: "child_one".to_string(),
                endpoints: Vec::new(),
            }),
        );
        let cluster_picker: Arc<dyn Picker> = Arc::new(ClusterPicker { children });
        let req = RequestHeaders::new();

        // Known cluster -> delegates to child picker and forwards CallOptions.
        assert_picks_child(&cluster_picker, "cluster_one", "child_one");

        // Unknown cluster -> debug_assert panic, or Drop(Internal) in release.
        let mut opts = CallOptions::new();
        opts.set_option(XdsCluster("cluster_unknown".into()));
        match cluster_picker.pick(PickOptions::new(&req, &opts)) {
            PickResult::Drop(err) => {
                assert_eq!(err.code(), StatusCodeError::Internal);
                assert!(err.message().contains("unknown cluster"));
            }
            other => panic!("expected Drop, got {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "cluster attribute not present")
    )]
    fn missing_cluster_attribute_is_internal_error() {
        let cluster_picker = ClusterPicker {
            children: HashMap::new(),
        };
        let req = RequestHeaders::new();
        let opts = CallOptions::new();
        match cluster_picker.pick(PickOptions::new(&req, &opts)) {
            PickResult::Drop(err) => {
                assert_eq!(err.code(), StatusCodeError::Internal);
                assert!(err.message().contains("not present"));
            }
            other => panic!("expected Drop, got {other:?}"),
        }
    }

    #[test]
    fn resolver_update_passes_endpoints_to_every_child() {
        let (mut policy, _scheduler, mut controller) = setup_test_policy();
        let json = serde_json::json!({
            "children": {
                "cluster_a": { "childPolicy": [{ "test_dummy_lb": { "tag": "child_a" } }] },
                "cluster_b": { "childPolicy": [{ "test_dummy_lb": { "tag": "child_b" } }] }
            }
        });
        let cfg = ClusterManagerBuilder
            .parse_config(&LbConfigJson::new(&json.to_string()).unwrap())
            .unwrap();
        let endpoints = vec![Endpoint::default()];
        let mut update = ResolverUpdate::default();
        update.endpoints = Ok(endpoints.clone());
        policy
            .resolver_update(update, &cfg, &mut controller)
            .unwrap();

        let state = controller.latest_state.take().expect("state update");
        assert_eq!(picked_endpoints(&state.picker, "cluster_a"), endpoints);
        assert_eq!(picked_endpoints(&state.picker, "cluster_b"), endpoints);
    }

    #[test]
    fn child_error_still_publishes_picker() {
        let (mut policy, _scheduler, mut controller) = setup_test_policy();
        let json = serde_json::json!({
            "children": {
                "cluster_a": { "childPolicy": [{ "test_dummy_lb": { "tag": "child_a" } }] },
                "cluster_b": { "childPolicy": [{ "test_dummy_lb": { "tag": "child_b", "fail_update": true } }] }
            }
        });
        let cfg = ClusterManagerBuilder
            .parse_config(&LbConfigJson::new(&json.to_string()).unwrap())
            .unwrap();

        let err = policy
            .resolver_update(ResolverUpdate::default(), &cfg, &mut controller)
            .unwrap_err();

        let state = controller.latest_state.take().expect("state update");
        assert_picks_child(&state.picker, "cluster_a", "child_a");
        assert_eq!(
            err,
            "failed to update xds_cluster_manager children: child \"cluster_b\": child_b failed"
        );
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "unknown cluster"))]
    fn cluster_removal_on_update() {
        let (mut policy, _scheduler, mut controller) = setup_test_policy();

        let state = apply_json_config(
            &mut policy,
            &mut controller,
            serde_json::json!({
                "children": {
                    "cluster_a": { "childPolicy": [{ "test_dummy_lb": { "tag": "child_a" } }] },
                    "cluster_b": { "childPolicy": [{ "test_dummy_lb": { "tag": "child_b" } }] }
                }
            }),
        );
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        assert_picks_child(&state.picker, "cluster_a", "child_a");
        assert_picks_child(&state.picker, "cluster_b", "child_b");

        // Drop cluster_b from the config.
        let state = apply_json_config(
            &mut policy,
            &mut controller,
            serde_json::json!({
                "children": {
                    "cluster_a": { "childPolicy": [{ "test_dummy_lb": { "tag": "child_a" } }] }
                }
            }),
        );

        assert_picks_child(&state.picker, "cluster_a", "child_a");

        let req = RequestHeaders::new();
        let mut opts_b = CallOptions::new();
        opts_b.set_option(XdsCluster("cluster_b".into()));
        match state.picker.pick(PickOptions::new(&req, &opts_b)) {
            PickResult::Drop(err) => assert_eq!(err.code(), StatusCodeError::Internal),
            other => panic!("expected Drop for removed cluster, got {other:?}"),
        }
    }

    #[test]
    fn work_delegates_to_child_and_refreshes_picker() {
        let (mut policy, scheduler, mut controller) = setup_test_policy();

        let initial = apply_json_config(
            &mut policy,
            &mut controller,
            serde_json::json!({
                "children": {
                    "cluster_a": {
                        "childPolicy": [{ "test_dummy_lb": { "tag": "work_ready", "start_connecting": true } }]
                    }
                }
            }),
        );
        assert_eq!(initial.connectivity_state, ConnectivityState::Connecting);

        let work_item = scheduler
            .queued_work
            .lock()
            .unwrap()
            .pop()
            .expect("scheduled work");
        policy.work(work_item, &mut controller);

        let updated = controller.latest_state.take().expect("updated state");
        assert_eq!(updated.connectivity_state, ConnectivityState::Ready);
        assert_picks_child(&updated.picker, "cluster_a", "work_ready");
    }

    #[test]
    fn exit_idle_wakes_children_and_refreshes_picker() {
        let (mut policy, _scheduler, mut controller) = setup_test_policy();

        let initial = apply_json_config(
            &mut policy,
            &mut controller,
            serde_json::json!({
                "children": {
                    "cluster_a": { "childPolicy": [{ "test_dummy_lb": { "tag": "woken_a", "start_idle": true } }] },
                    "cluster_b": { "childPolicy": [{ "test_dummy_lb": { "tag": "woken_b", "start_idle": true } }] }
                }
            }),
        );
        assert_eq!(initial.connectivity_state, ConnectivityState::Idle);

        policy.exit_idle(&mut controller);

        let woken = controller.latest_state.take().expect("woken state");
        assert_eq!(woken.connectivity_state, ConnectivityState::Ready);
        assert_picks_child(&woken.picker, "cluster_a", "woken_a");
        assert_picks_child(&woken.picker, "cluster_b", "woken_b");
    }

    fn setup_test_policy() -> (
        ClusterManagerPolicy,
        Arc<MockScheduler>,
        MockChannelController,
    ) {
        GLOBAL_LB_REGISTRY.add_builder(TestDummyLbBuilder);
        let scheduler = Arc::new(MockScheduler::default());
        let policy = ClusterManagerBuilder.build(LbPolicyOptions {
            work_scheduler: scheduler.clone(),
            runtime: default_runtime(),
        });
        let controller = MockChannelController { latest_state: None };
        (policy, scheduler, controller)
    }

    fn apply_json_config(
        policy: &mut ClusterManagerPolicy,
        controller: &mut MockChannelController,
        json: serde_json::Value,
    ) -> LbState {
        let cfg = ClusterManagerBuilder
            .parse_config(&LbConfigJson::new(&json.to_string()).unwrap())
            .unwrap();
        policy
            .resolver_update(ResolverUpdate::default(), &cfg, controller)
            .unwrap();
        controller.latest_state.take().expect("state update")
    }

    fn assert_picks_child(picker: &Arc<dyn Picker>, cluster: &str, expected_tag: &str) {
        let req = RequestHeaders::new();
        let picked = PickedChild::default();
        let mut opts = CallOptions::new();
        opts.set_option(XdsCluster(cluster.into()));
        opts.set_option(picked.clone());
        assert!(matches!(
            picker.pick(PickOptions::new(&req, &opts)),
            PickResult::Queue
        ));
        assert_eq!(picked.0.lock().unwrap().as_deref(), Some(expected_tag));
    }

    // Picks `cluster` and returns the endpoints its child received.
    fn picked_endpoints(picker: &Arc<dyn Picker>, cluster: &str) -> Vec<Endpoint> {
        let req = RequestHeaders::new();
        let endpoints = ChildEndpoints::default();
        let mut opts = CallOptions::new();
        opts.set_option(XdsCluster(cluster.into()));
        opts.set_option(endpoints.clone());
        assert!(matches!(
            picker.pick(PickOptions::new(&req, &opts)),
            PickResult::Queue
        ));
        endpoints
            .0
            .lock()
            .unwrap()
            .clone()
            .expect("child endpoints attribute")
    }

    #[derive(Debug)]
    struct DummyPicker;

    impl Picker for DummyPicker {
        fn pick(&self, _options: PickOptions<'_>) -> PickResult {
            PickResult::Queue
        }
    }

    #[derive(Clone, Debug, Default)]
    struct PickedChild(Arc<Mutex<Option<String>>>);

    // Endpoints the picked child received in its last resolver update.
    #[derive(Clone, Debug, Default)]
    struct ChildEndpoints(Arc<Mutex<Option<Vec<Endpoint>>>>);

    #[derive(Debug)]
    struct TaggedPicker {
        tag: String,
        endpoints: Vec<Endpoint>,
    }

    impl Picker for TaggedPicker {
        fn pick(&self, options: PickOptions<'_>) -> PickResult {
            if let Some(picked) = options.call_options.get_option::<PickedChild>() {
                *picked.0.lock().unwrap() = Some(self.tag.clone());
            }
            if let Some(endpoints) = options.call_options.get_option::<ChildEndpoints>() {
                *endpoints.0.lock().unwrap() = Some(self.endpoints.clone());
            }
            PickResult::Queue
        }
    }

    #[derive(Debug, Default)]
    struct MockScheduler {
        queued_work: Mutex<Vec<Option<WorkData>>>,
    }

    impl WorkScheduler for MockScheduler {
        fn schedule_work(&self, data: Option<WorkData>) {
            self.queued_work.lock().unwrap().push(data);
        }
    }

    struct MockChannelController {
        latest_state: Option<LbState>,
    }

    impl ChannelController for MockChannelController {
        fn new_subchannel(
            &mut self,
            _address: &Address,
            _work_scheduler: Arc<dyn WorkScheduler>,
        ) -> (Arc<dyn Subchannel>, SubchannelState) {
            unimplemented!()
        }

        fn update_picker(&mut self, update: LbState) {
            self.latest_state = Some(update);
        }

        fn request_resolution(&mut self) {}
    }

    #[derive(Debug, Default, Deserialize)]
    struct TestDummyConfig {
        #[serde(default)]
        tag: String,
        #[serde(default)]
        start_connecting: bool,
        #[serde(default)]
        start_idle: bool,
        #[serde(default)]
        fail_update: bool,
    }

    #[derive(Debug)]
    struct TestDummyLbPolicy {
        work_scheduler: Arc<dyn WorkScheduler>,
        tag: String,
        endpoints: Vec<Endpoint>,
    }

    impl TestDummyLbPolicy {
        fn tagged_picker(&self) -> Arc<dyn Picker> {
            Arc::new(TaggedPicker {
                tag: self.tag.clone(),
                endpoints: self.endpoints.clone(),
            })
        }
    }

    impl LbPolicy for TestDummyLbPolicy {
        type LbConfig = TestDummyConfig;

        fn resolver_update(
            &mut self,
            update: ResolverUpdate,
            config: &Self::LbConfig,
            channel_controller: &mut dyn ChannelController,
        ) -> Result<(), String> {
            self.tag = config.tag.clone();
            self.endpoints = update.endpoints.unwrap_or_default();
            if config.fail_update {
                return Err(format!("{} failed", config.tag));
            }
            if config.start_connecting {
                channel_controller.update_picker(LbState {
                    connectivity_state: ConnectivityState::Connecting,
                    picker: Arc::new(DummyPicker),
                });
                self.work_scheduler.schedule_work(None);
            } else if config.start_idle {
                channel_controller.update_picker(LbState {
                    connectivity_state: ConnectivityState::Idle,
                    picker: Arc::new(DummyPicker),
                });
            } else {
                channel_controller.update_picker(LbState {
                    connectivity_state: ConnectivityState::Ready,
                    picker: self.tagged_picker(),
                });
            }
            Ok(())
        }

        fn work(
            &mut self,
            _data: Option<WorkData>,
            channel_controller: &mut dyn ChannelController,
        ) {
            channel_controller.update_picker(LbState {
                connectivity_state: ConnectivityState::Ready,
                picker: self.tagged_picker(),
            });
        }

        fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
            channel_controller.update_picker(LbState {
                connectivity_state: ConnectivityState::Ready,
                picker: self.tagged_picker(),
            });
        }
    }

    #[derive(Debug)]
    struct TestDummyLbBuilder;

    impl LbPolicyBuilder for TestDummyLbBuilder {
        type LbPolicy = TestDummyLbPolicy;

        fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
            TestDummyLbPolicy {
                work_scheduler: options.work_scheduler,
                tag: String::new(),
                endpoints: Vec::new(),
            }
        }

        fn name(&self) -> &'static str {
            "test_dummy_lb"
        }

        fn parse_config(&self, config: &LbConfigJson) -> Result<TestDummyConfig, String> {
            config.convert_to().map_err(|e| e.to_string())
        }
    }
}
