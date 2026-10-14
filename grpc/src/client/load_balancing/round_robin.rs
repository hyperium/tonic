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

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::FailingPicker;
use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::PickOptions;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::child_manager::ChildManager;
use crate::client::load_balancing::child_manager::ChildUpdate;
use crate::client::load_balancing::pick_first::PickFirstBuilder;
use crate::client::load_balancing::pick_first::PickFirstConfig;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::ResolverUpdate;

pub static POLICY_NAME: &str = "round_robin";
static START: Once = Once::new();

#[derive(Debug, Default)]
pub(crate) struct RoundRobinConfig(PickFirstConfig);

#[derive(Debug)]
pub(crate) struct RoundRobinBuilder {}

impl LbPolicyBuilder for RoundRobinBuilder {
    type LbPolicy = RoundRobinPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        let child_manager = ChildManager::new(options.runtime, options.work_scheduler);
        RoundRobinPolicy::new(child_manager)
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }

    fn parse_config(
        &self,
        _config: &super::LbConfigJson,
    ) -> Result<<Self::LbPolicy as LbPolicy>::LbConfig, String> {
        Ok(RoundRobinConfig::default())
    }
}

#[derive(Debug)]
pub(crate) struct RoundRobinPolicy {
    child_manager: ChildManager<Endpoint, PickFirstBuilder>,
}

impl RoundRobinPolicy {
    fn new(child_manager: ChildManager<Endpoint, PickFirstBuilder>) -> Self {
        Self { child_manager }
    }

    // Sets the policy's state to TRANSIENT_FAILURE with a picker returning the
    // error string provided, then requests re-resolution from the channel.
    fn move_to_transient_failure(
        &mut self,
        error: String,
        channel_controller: &mut dyn ChannelController,
    ) {
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::TransientFailure,
            picker: Arc::new(FailingPicker { error }),
        });
        channel_controller.request_resolution();
    }

    // Sends an aggregate picker based on states of children.
    //
    // The state is determined according to normal state aggregation rules, and
    // the picker round-robins between all children in that state.
    fn update_picker(&mut self, channel_controller: &mut dyn ChannelController) {
        if !self.child_manager.child_updated() {
            return;
        }
        let aggregate_state = self.child_manager.aggregate_states();
        let pickers = self
            .child_manager
            .children()
            .map(|(_, child)| child.state())
            .filter(|state| state.connectivity_state == aggregate_state)
            .map(|state| state.picker.clone())
            .collect();
        let picker_update = LbState {
            connectivity_state: aggregate_state,
            picker: Arc::new(RoundRobinPicker::new(pickers)),
        };
        channel_controller.update_picker(picker_update);
    }

    // Responds to an incoming ResolverUpdate containing an Err in endpoints by
    // forwarding it to all children unconditionally.  Updates the picker as
    // needed.
    fn handle_resolver_error(
        &mut self,
        resolver_update: ResolverUpdate,
        config: &RoundRobinConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let err = format!(
            "Received error from name resolver: {}",
            resolver_update.endpoints.as_ref().unwrap_err()
        );
        if self.child_manager.children().next().is_none() {
            // We had no children so we must produce an erroring picker.
            self.move_to_transient_failure(err.clone(), channel_controller);
            return Err(err);
        }
        // Forward the error to each child, ignoring their responses.
        let _ = self
            .child_manager
            .resolver_update(resolver_update, &config.0, channel_controller);
        self.update_picker(channel_controller);
        Err(err)
    }
}

impl LbPolicy for RoundRobinPolicy {
    type LbConfig = RoundRobinConfig;
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        if update.endpoints.is_err() {
            return self.handle_resolver_error(update, config, channel_controller);
        }

        // Shard the update by endpoint.
        let updates = update.endpoints.as_ref().unwrap().iter().map(|e| {
            let update = ResolverUpdate {
                attributes: crate::attributes::Attributes::default(),
                endpoints: Ok(vec![e.clone()]),
                service_config: update.service_config.clone(),
                resolution_note: None,
            };
            ChildUpdate {
                child_identifier: e.clone(),
                child_policy_builder: PickFirstBuilder {},
                child_update: Some((update, &config.0)),
            }
        });
        self.child_manager
            .update(updates, channel_controller)
            .unwrap();

        if self.child_manager.children().next().is_none() {
            // There are no children remaining, so report this error and produce
            // an erroring picker.
            let err = "Received empty address list from the name resolver";
            self.move_to_transient_failure(err.into(), channel_controller);
            return Err(err.into());
        }

        self.update_picker(channel_controller);
        Ok(())
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        self.child_manager.work(data, channel_controller);
        self.update_picker(channel_controller);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.child_manager.exit_idle(channel_controller);
        self.update_picker(channel_controller);
    }
}

/// Register round robin as a LbPolicy.
pub(crate) fn reg() {
    START.call_once(|| {
        GLOBAL_LB_REGISTRY.add_builder(RoundRobinBuilder {});
    });
}

#[derive(Debug)]
struct RoundRobinPicker {
    pickers: Vec<Arc<dyn Picker>>,
    next: AtomicUsize,
}

impl RoundRobinPicker {
    fn new(pickers: Vec<Arc<dyn Picker>>) -> Self {
        let random_index: usize = rand::random_range(..pickers.len());
        Self {
            pickers,
            next: AtomicUsize::new(random_index),
        }
    }
}

impl Picker for RoundRobinPicker {
    fn pick(&self, options: PickOptions<'_>) -> PickResult {
        let len = self.pickers.len();
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % len;
        self.pickers[idx].pick(options)
    }
}

#[cfg(test)]
mod test {
    use std::panic;

    use super::*;
    use crate::StatusCodeError;
    use crate::call_attributes::CallAttributes;
    use crate::client::load_balancing::subchannel::Subchannel;
    use crate::client::load_balancing::subchannel::SubchannelState;
    use crate::client::load_balancing::test_utils;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::core::Address;
    use crate::rt::default_runtime;

    // Constructs the test environment for RoundRobinPolicy tests.
    fn new_env() -> TestEnv<RoundRobinPolicy> {
        TestEnv::new(|work_scheduler| {
            RoundRobinPolicy::new(ChildManager::new(default_runtime(), work_scheduler))
        })
    }

    fn create_endpoints(num_endpoints: usize) -> Vec<Endpoint> {
        let mut endpoints = Vec::with_capacity(num_endpoints);
        for i in 0..num_endpoints {
            let addresses = vec![Address {
                address: format!("{}.{}.{}.{}:{}", i + 1, i + 1, i + 1, i + 1, 1).into(),
                ..Default::default()
            }];
            endpoints.push(Endpoint {
                addresses,
                ..Default::default()
            });
        }
        endpoints
    }

    // Creates a new endpoint with the specified number of addresses.
    fn create_endpoint(num_addresses: usize) -> Endpoint {
        let mut addresses = Vec::with_capacity(num_addresses);
        for i in 0..num_addresses {
            addresses.push(Address {
                address: format!("{}.{}.{}.{}:{}", i, i, i, i, i).into(),
                ..Default::default()
            });
        }
        Endpoint {
            addresses,
            ..Default::default()
        }
    }

    impl TestEnv<RoundRobinPolicy> {
        // Simulates a state change of `subchannel` to TRANSIENT_FAILURE with
        // the given connection error, and delivers the resulting work to the
        // policy.
        fn move_subchannel_to_transient_failure(
            &mut self,
            subchannel: &Arc<dyn Subchannel>,
            err: &str,
        ) {
            self.send_subchannel_update(
                subchannel,
                &SubchannelState {
                    connectivity_state: ConnectivityState::TransientFailure,
                    last_connection_error: Some(err.into()),
                },
            );
        }

        // Verifies that the expected number of subchannels is created. Returns
        // the subchannels created.
        fn verify_subchannel_creation(
            &mut self,
            number_of_subchannels: usize,
        ) -> Vec<Arc<dyn Subchannel>> {
            let mut subchannels = Vec::new();
            for _ in 0..number_of_subchannels {
                subchannels.push(self.expect_new_subchannel());
                self.expect_connect();
            }
            subchannels
        }

        // Verifies that the channel moves to CONNECTING state with a queuing
        // picker.
        //
        // Returns the picker for tests to make more picks, if required.
        fn verify_connecting_picker(&mut self) -> Arc<dyn Picker> {
            println!("verify connecting picker");
            let update = self.expect_picker_update();
            println!("connectivity state is {}", update.connectivity_state);
            assert!(update.connectivity_state == ConnectivityState::Connecting);
            let req = test_utils::new_request_headers();
            assert!(
                update
                    .picker
                    .pick(PickOptions::new(&req, &CallAttributes::new()))
                    == PickResult::Queue
            );
            update.picker
        }

        // Verifies that the channel moves to READY state with a picker that
        // returns the given subchannel.
        //
        // Returns the picker for tests to make more picks, if required.
        fn verify_ready_picker(&mut self, subchannel: Arc<dyn Subchannel>) -> Arc<dyn Picker> {
            println!("verify ready picker");
            let update = self.expect_picker_update();

            println!(
                "connectivity state for ready picker is {}",
                update.connectivity_state
            );
            assert_eq!(update.connectivity_state, ConnectivityState::Ready);

            let req = test_utils::new_request_headers();
            let PickResult::Pick(pick) = update
                .picker
                .pick(PickOptions::new(&req, &CallAttributes::new()))
            else {
                panic!("unexpected pick result");
            };

            println!("selected subchannel is {}", pick.subchannel);
            println!("should've been selected subchannel is {}", subchannel);
            assert_eq!(&pick.subchannel, &subchannel);

            update.picker
        }

        // Returns the picker for when there are multiple pickers in the ready
        // picker.
        fn verify_roundrobin_ready_picker(&mut self) -> Arc<dyn Picker> {
            println!("verify ready picker");
            let update = self.expect_picker_update();

            println!(
                "connectivity state for ready picker is {}",
                update.connectivity_state
            );
            assert_eq!(update.connectivity_state, ConnectivityState::Ready);

            let req = test_utils::new_request_headers();
            let result = update
                .picker
                .pick(PickOptions::new(&req, &CallAttributes::new()));
            assert!(
                matches!(result, PickResult::Pick(_)),
                "unexpected pick result {result:?}"
            );

            update.picker
        }

        // Verifies that the channel moves to TRANSIENT_FAILURE state with a
        // picker that returns an error with the given message. The error code
        // should be UNAVAILABLE..
        //
        // Returns the picker for tests to make more picks, if required.
        fn verify_transient_failure_picker(&mut self, want_error: String) -> Arc<dyn Picker> {
            let update = self.expect_picker_update();

            assert_eq!(
                update.connectivity_state,
                ConnectivityState::TransientFailure
            );

            let req = test_utils::new_request_headers();
            let PickResult::Fail(status) = update
                .picker
                .pick(PickOptions::new(&req, &CallAttributes::new()))
            else {
                panic!("unexpected pick result");
            };

            assert_eq!(status.code(), StatusCodeError::Unavailable);
            assert!(
                status.message().contains(&want_error),
                "expected error message to contain {:?}, got {:?}",
                want_error,
                status.message()
            );

            update.picker
        }
    }

    // Tests the scenario where the resolver returns an error before a valid
    // update. The LB policy should move to TRANSIENT_FAILURE state with a
    // failing picker.
    #[tokio::test]
    async fn roundrobin_resolver_error_before_a_valid_update() {
        let mut env = new_env();
        let resolver_error = String::from("resolver error");
        assert!(env.send_resolver_error(resolver_error.clone()).is_err());
        env.verify_transient_failure_picker(resolver_error);
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy has moved to READY. The LB policy should ignore the error
    // and continue using the previously received update.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_ready() {
        let mut env = new_env();
        let endpoint = create_endpoint(1);
        env.send_resolver_update(vec![endpoint]).unwrap();
        let subchannels = env.verify_subchannel_creation(1);
        env.verify_connecting_picker();

        env.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
        let picker = env.verify_ready_picker(subchannels[0].clone());
        let resolver_error = String::from("resolver error");
        assert!(env.send_resolver_error(resolver_error.clone()).is_err());
        env.expect_no_events();

        let req = test_utils::new_request_headers();
        match picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
            PickResult::Pick(pick) => {
                assert!(pick.subchannel == subchannels[0].clone());
            }
            other => panic!("unexpected pick result {}", other),
        }
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy is still trying to connect. The LB policy should ignore the
    // error and continue using the previously received update.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_connecting() {
        let mut env = new_env();

        let endpoint = create_endpoint(1);
        env.send_resolver_update(vec![endpoint]).unwrap();
        env.verify_subchannel_creation(1);
        let picker = env.verify_connecting_picker();

        let resolver_error = String::from("resolver error");

        assert!(env.send_resolver_error(resolver_error).is_err());

        env.expect_no_events();

        let req = test_utils::new_request_headers();
        match picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
            PickResult::Queue => {}
            other => panic!("unexpected pick result {}", other),
        }
    }

    // Tests the scenario where the resolver returns an error after a valid
    // update and the LB policy has moved to TRANSIENT_FAILURE after attempting
    // to connect to all addresses. The LB policy should send a new picker that
    // returns the error from the resolver.
    #[tokio::test]
    async fn roundrobin_resolver_error_after_a_valid_update_in_tf() {
        let mut env = new_env();
        let endpoint = create_endpoint(1);
        env.send_resolver_update(vec![endpoint]).unwrap();
        let subchannels = env.verify_subchannel_creation(1);
        env.verify_connecting_picker();
        let connection_error = String::from("test connection error");
        env.move_subchannel_to_transient_failure(&subchannels[0], &connection_error);
        env.expect_request_resolution();
        env.verify_transient_failure_picker(connection_error);
        let resolver_error = String::from("resolver error");
        assert!(env.send_resolver_error(resolver_error.clone()).is_err());
        env.expect_request_resolution();
        env.verify_transient_failure_picker(resolver_error);
    }

    // Round Robin should round robin across endpoints.
    #[tokio::test]
    async fn roundrobin_picks_are_round_robin() {
        let mut env = new_env();
        let endpoints = create_endpoints(2);
        env.send_resolver_update(endpoints).unwrap();
        let subchannels = env.verify_subchannel_creation(2);
        env.verify_connecting_picker();
        env.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
        env.verify_ready_picker(subchannels[0].clone());
        env.send_subchannel_update(&subchannels[1], &SubchannelState::ready());
        let picker = env.verify_roundrobin_ready_picker();
        let req = test_utils::new_request_headers();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
                PickResult::Pick(pick) => {
                    println!("picked subchannel is {}", pick.subchannel);
                    picked.push(pick.subchannel.clone());
                }
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(
            picked[0] != picked[1].clone(),
            "Should alternate between subchannels"
        );
        assert_eq!(&picked[0], &picked[2]);
        assert_eq!(&picked[1], &picked[3]);
        assert!(picked.contains(&subchannels[0]));
        assert!(picked.contains(&subchannels[1]));
    }

    // If round robin receives no endpoints in a resolver update,
    // it should go into transient failure.
    #[tokio::test]
    async fn roundrobin_endpoints_removed() {
        let mut env = new_env();

        let endpoints = create_endpoints(2);
        env.send_resolver_update(endpoints).unwrap();
        let _subchannels = env.verify_subchannel_creation(2);
        env.verify_connecting_picker();

        assert!(env.send_resolver_update(vec![]).is_err());
        let want_error = "Received empty address list from the name resolver";
        env.verify_transient_failure_picker(want_error.to_string());
        env.expect_request_resolution();
    }

    // Round robin should only round robin across children that are ready.
    // If a child leaves the ready state, Round Robin should only
    // pick from the children that are still Ready.
    #[tokio::test]
    async fn roundrobin_one_endpoint_down() {
        let mut env = new_env();
        let endpoints = create_endpoints(2);
        env.send_resolver_update(endpoints).unwrap();
        let subchannels = env.verify_subchannel_creation(2);
        env.verify_connecting_picker();
        env.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
        let _picker = env.verify_ready_picker(subchannels[0].clone());
        env.send_subchannel_update(&subchannels[1], &SubchannelState::ready());
        let picker = env.verify_roundrobin_ready_picker();
        let req = test_utils::new_request_headers();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
                PickResult::Pick(pick) => {
                    println!("picked subchannel is {}", pick.subchannel);
                    picked.push(pick.subchannel.clone());
                }
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(
            picked[0] != picked[1].clone(),
            "Should alternate between subchannels"
        );
        assert_eq!(&picked[0], &picked[2]);
        assert_eq!(&picked[1], &picked[3]);

        assert!(picked.contains(&subchannels[0]));
        assert!(picked.contains(&subchannels[1]));
        let subchannel_being_removed = subchannels[1].clone();
        let error = "endpoint down";
        env.move_subchannel_to_transient_failure(&subchannels[1], error);

        let new_picker = env.verify_roundrobin_ready_picker();

        let req = test_utils::new_request_headers();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match new_picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
                PickResult::Pick(pick) => {
                    println!("picked subchannel is {}", pick.subchannel);
                    picked.push(pick.subchannel.clone());
                }
                other => panic!("unexpected pick result {}", other),
            }
        }

        assert_eq!(&picked[0], &picked[2]);
        assert_eq!(&picked[1], &picked[3]);
        assert!(picked.contains(&subchannels[0]));
        assert!(!picked.contains(&subchannel_being_removed));
    }

    // If Round Robin receives a resolver update that removes an endpoint and
    // adds a new endpoint from a previous update, that endpoint's subchannels
    // should not be a part of its picks anymore and should be removed. It should
    // then roundrobin across the endpoints it still has and the new one.
    #[tokio::test]
    async fn roundrobin_pick_after_resolved_updated_hosts() {
        let mut env = new_env();

        // Two initial endpoints: subchannel_one, subchannel_two
        let addr_one = Address {
            address: "subchannel_one".to_string().into(),
            ..Default::default()
        };
        let addr_two = Address {
            address: "subchannel_two".to_string().into(),
            ..Default::default()
        };
        let endpoint_one = Endpoint {
            addresses: vec![addr_one],
            ..Default::default()
        };
        let endpoint_two = Endpoint {
            addresses: vec![addr_two],
            ..Default::default()
        };

        env.send_resolver_update(vec![endpoint_one, endpoint_two.clone()])
            .unwrap();

        // Start with two subchannels created
        let all_subchannels = env.verify_subchannel_creation(2);
        env.verify_connecting_picker();
        let subchannel_one = all_subchannels
            .iter()
            .find(|sc| sc.address().address == "subchannel_one".to_string().into())
            .unwrap();
        let subchannel_two = all_subchannels
            .iter()
            .find(|sc| sc.address().address == "subchannel_two".to_string().into())
            .unwrap();

        env.send_subchannel_update(subchannel_one, &SubchannelState::ready());
        env.verify_ready_picker(subchannel_one.clone());
        env.send_subchannel_update(subchannel_two, &SubchannelState::ready());
        let picker = env.verify_roundrobin_ready_picker();

        let req = test_utils::new_request_headers();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
                PickResult::Pick(pick) => picked.push(pick.subchannel.clone()),
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(picked.contains(subchannel_one));
        assert!(picked.contains(subchannel_two));

        // Resolver update removes subchannel_one and adds "new"
        let new_addr = Address {
            address: "new".to_string().into(),
            ..Default::default()
        };
        let new_endpoint = Endpoint {
            addresses: vec![new_addr],
            ..Default::default()
        };

        env.send_resolver_update(vec![endpoint_two, new_endpoint])
            .unwrap();

        // Only 1 new subchannel is created for new_endpoint; endpoint_two is
        // retained.
        let new_subchannels = env.verify_subchannel_creation(1);
        let new_sc = &new_subchannels[0];
        let old_sc = subchannel_two;

        // Round Robin sends a picker update with the currently ready retained
        // subchannel.
        let _ = env.verify_ready_picker(old_sc.clone());

        env.send_subchannel_update(new_sc, &SubchannelState::ready());
        let new_picker = env.verify_roundrobin_ready_picker();

        let req = test_utils::new_request_headers();
        let mut picked = Vec::new();
        for _ in 0..4 {
            match new_picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
                PickResult::Pick(pick) => picked.push(pick.subchannel.clone()),
                other => panic!("unexpected pick result {}", other),
            }
        }
        assert!(picked.contains(old_sc));
        assert!(picked.contains(new_sc));
        assert!(!picked.contains(subchannel_one));
    }

    // Round robin should stay in transient failure until a child reports ready
    #[tokio::test]
    async fn roundrobin_stay_transient_failure_until_ready() {
        let mut env = new_env();
        let endpoints = create_endpoints(2);
        env.send_resolver_update(endpoints).unwrap();
        let subchannels = env.verify_subchannel_creation(2);
        env.verify_connecting_picker();

        let first_error = String::from("test connection error 1");
        env.move_subchannel_to_transient_failure(&subchannels[0], &first_error);
        env.expect_request_resolution();
        env.verify_connecting_picker();
        env.move_subchannel_to_transient_failure(&subchannels[1], &first_error);
        env.expect_request_resolution();
        env.verify_transient_failure_picker(first_error);
        env.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
        env.verify_ready_picker(subchannels[0].clone());
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // (before sending any valid update). The LB policy should move to
    // TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn roundrobin_zero_endpoints_from_resolver_before_valid_update() {
        let mut env = new_env();
        assert!(env.send_resolver_update(vec![]).is_err());
        env.verify_transient_failure_picker(
            "Received empty address list from the name resolver".to_string(),
        );
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // after sending a valid update (and the LB policy has moved to READY). The LB
    // policy should move to TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn roundrobin_zero_endpoints_from_resolver_after_valid_update() {
        let mut env = new_env();

        let endpoint = create_endpoint(1);
        env.send_resolver_update(vec![endpoint]).unwrap();
        let subchannels = env.verify_subchannel_creation(1);
        env.verify_connecting_picker();
        env.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
        env.verify_ready_picker(subchannels[0].clone());
        assert!(env.send_resolver_update(vec![]).is_err());
        env.verify_transient_failure_picker(
            "Received empty address list from the name resolver".to_string(),
        );
        env.expect_request_resolution();
    }

    // Tests the scenario where the resolver returns an update with multiple
    // address. The LB policy should create subchannels for all address, and attempt
    // to connect to them in order, until a connection succeeds, at which point it
    // should move to READY state with a picker that returns that subchannel.
    #[tokio::test]
    async fn roundrobin_with_multiple_backends_first_backend_is_ready() {
        let mut env = new_env();

        let endpoint = create_endpoints(2);
        env.send_resolver_update(endpoint).unwrap();
        let subchannels = env.verify_subchannel_creation(2);
        env.verify_connecting_picker();

        env.send_subchannel_update(&subchannels[0], &SubchannelState::ready());

        let picker = env.verify_ready_picker(subchannels[0].clone());

        let req = test_utils::new_request_headers();
        // First pick determines the only subchannel the picker should yield
        let first_sc = match picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
            PickResult::Pick(p) => p.subchannel.clone(),
            other => panic!("unexpected pick result {}", other),
        };

        for _ in 0..7 {
            match picker.pick(PickOptions::new(&req, &CallAttributes::new())) {
                PickResult::Pick(p) => {
                    assert!(
                        Arc::ptr_eq(&first_sc, &p.subchannel),
                        "READY picker should contain exactly one subchannel"
                    );
                }
                other => panic!("unexpected pick result {}", other),
            }
        }
    }

    // Tests the scenario where the resolver returns an update with multiple
    // endpoints and the LB policy successfully connects to the first one and
    // moves to READY. The resolver then returns an update with a new endpoint
    // list that contains the currently connected endpoint. The LB policy should
    // create subchannels for the new endpoints, and then see that the currently
    // connected endpoint is in the new endpoint list. It should then send a new
    // READY picker that returns the currently connected endpoint.
    #[tokio::test]
    async fn roundrobin_resolver_update_contains_currently_ready_subchannel() {
        let mut env = new_env();

        let endpoints = create_endpoints(2);
        env.send_resolver_update(endpoints).unwrap();
        let subchannels = env.verify_subchannel_creation(2);
        env.verify_connecting_picker();
        env.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
        env.verify_ready_picker(subchannels[0].clone());

        let mut endpoints = create_endpoints(4);
        endpoints.reverse();
        env.send_resolver_update(endpoints).unwrap();
        // subchannel 0 (1.1.1.1:1) and subchannel 1 (2.2.2.2:1) are retained.
        // Two new subchannels are created (3.3.3.3:1 and 4.4.4.4:1).
        let _new_subchannels = env.verify_subchannel_creation(2);
        // Subchannel 1 (2.2.2.2:1) is retained but still idle, so it is asked
        // to connect again.  Subchannel 0 is already ready and is left alone.
        assert_eq!(env.expect_connect(), subchannels[1].address());
        // RoundRobin sends a new ready picker containing the currently ready
        // subchannel 0.
        env.verify_ready_picker(subchannels[0].clone());
    }
}
