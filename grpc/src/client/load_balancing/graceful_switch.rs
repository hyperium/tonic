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

use std::sync::Arc;

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::DynLbConfig;
use crate::client::load_balancing::DynLbPolicyBuilder;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::child_manager::ChildManager;
use crate::client::load_balancing::child_manager::ChildUpdate;
use crate::client::name_resolution::ResolverUpdate;
use crate::rt::GrpcRuntime;

#[derive(Debug, Clone)]
pub struct GracefulSwitchLbConfig {
    child_builder: Arc<DynLbPolicyBuilder>,
    child_config: DynLbConfig,
}

impl GracefulSwitchLbConfig {
    /// Creates a new [`GracefulSwitchLbConfig`].
    pub fn new(child_builder: Arc<DynLbPolicyBuilder>, child_config: DynLbConfig) -> Self {
        Self {
            child_builder,
            child_config,
        }
    }
}

/// A graceful switching load balancing policy.  In graceful switch, there is
/// always either one or two child policies.  When there is one policy, all
/// operations are delegated to it.  When the child policy type needs to change,
/// graceful switch creates a "pending" child policy alongside the "active"
/// policy.  When the pending policy leaves the CONNECTING state, or when the
/// active policy is not READY, graceful switch will promote the pending policy
/// to active and tear down the previously active policy.
#[derive(Debug)]
pub struct GracefulSwitchPolicy {
    child_manager: ChildManager<()>, // Child ID empty - only the name of the child LB policy matters.
    last_update: Option<LbState>, // Saves the last output LbState to determine if an update is needed.
    active_child_builder: Option<Arc<DynLbPolicyBuilder>>,
}

impl LbPolicy for GracefulSwitchPolicy {
    type LbConfig = GracefulSwitchLbConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        if self.active_child_builder.is_none() {
            // When there are no children yet, the current update immediately
            // becomes the active child.
            self.active_child_builder = Some(config.child_builder.clone());
        }
        let active_child_builder = self.active_child_builder.as_ref().unwrap();

        let mut children = Vec::with_capacity(2);

        // Always include the incoming update.
        children.push(ChildUpdate {
            child_policy_builder: config.child_builder.clone(),
            child_identifier: (),
            child_update: Some((update, &config.child_config)),
        });

        // Include the active child if it does not match the updated child so
        // that the child manager will not delete it.
        if config.child_builder.name() != active_child_builder.name() {
            children.push(ChildUpdate {
                child_policy_builder: active_child_builder.clone(),
                child_identifier: (),
                child_update: None,
            });
        }

        let res = self.child_manager.update(children, channel_controller);
        self.update_picker(channel_controller);
        res
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

#[derive(Debug, PartialEq, Eq, Clone)]
enum ChildKind {
    Current,
    Pending,
}

impl GracefulSwitchPolicy {
    /// Creates a new Graceful Switch policy.
    pub fn new(runtime: GrpcRuntime, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        GracefulSwitchPolicy {
            child_manager: ChildManager::new(runtime, work_scheduler),
            last_update: None,
            active_child_builder: None,
        }
    }

    fn update_picker(&mut self, channel_controller: &mut dyn ChannelController) {
        // If maybe_swap returns a None, then no update needs to happen.
        let Some(update) = self.maybe_swap(channel_controller) else {
            return;
        };
        // If the current update is the same as the last update, skip it.
        if self.last_update.as_ref().is_some_and(|lu| lu == &update) {
            return;
        }
        channel_controller.update_picker(update.clone());
        self.last_update = Some(update);
    }

    // Determines the appropriate state to output
    fn maybe_swap(&mut self, channel_controller: &mut dyn ChannelController) -> Option<LbState> {
        // If no child updated itself, there is nothing we can do.
        if !self.child_manager.child_updated() {
            return None;
        }

        // If resolver_update has never been called, we have no children, so
        // there's nothing we can do.
        let Some(active_child_builder) = &self.active_child_builder else {
            return None;
        };
        let active_name = active_child_builder.name();

        // Scan through the child manager's children for the active and
        // (optional) pending child.
        let mut active_child = None;
        let mut pending_child = None;
        for child in self.child_manager.children() {
            if child.builder.name() == active_name {
                active_child = Some(child);
            } else {
                pending_child = Some(child);
            }
        }
        let active_child = active_child.expect("There should always be an active child policy");

        // If no pending child exists, we will update the active child's state.
        let Some(pending_child) = pending_child else {
            return Some(active_child.state.clone());
        };

        // If the active child is still reading and the pending child is still
        // connecting, keep using the active child's state.
        if active_child.state.connectivity_state == ConnectivityState::Ready
            && pending_child.state.connectivity_state == ConnectivityState::Connecting
        {
            return Some(active_child.state.clone());
        }

        // Transition to the pending child and remove the active child.

        // Clone some things from child_manager.children to release the
        // child_manager reference.
        let pending_child_builder = pending_child.builder.clone();
        let pending_state = pending_child.state.clone();

        self.active_child_builder = Some(pending_child_builder.clone());
        self.child_manager
            .retain_children([((), pending_child_builder)]);

        Some(pending_state)
    }
}

#[cfg(test)]
mod test {
    use std::panic;
    use std::sync::Arc;
    use std::sync::mpsc;

    use crate::client::RequestHeaders;
    use crate::client::load_balancing::ChannelController;
    use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
    use crate::client::load_balancing::LbPolicy;
    use crate::client::load_balancing::LbState;
    use crate::client::load_balancing::Pick;
    use crate::client::load_balancing::PickResult;
    use crate::client::load_balancing::Picker;
    use crate::client::load_balancing::Subchannel;
    use crate::client::load_balancing::SubchannelState;
    use crate::client::load_balancing::WorkScheduler;
    use crate::client::load_balancing::graceful_switch::GracefulSwitchLbConfig;
    use crate::client::load_balancing::graceful_switch::GracefulSwitchPolicy;
    use crate::client::load_balancing::subchannel::SubchannelUpdate;
    use crate::client::load_balancing::test_utils::StubPolicyData;
    use crate::client::load_balancing::test_utils::StubPolicyFuncs;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::client::load_balancing::test_utils::TestSubchannel;
    use crate::client::load_balancing::test_utils::reg_stub_policy;
    use crate::client::load_balancing::test_utils::{self};
    use crate::client::name_resolution::Endpoint;
    use crate::client::name_resolution::ResolverUpdate;
    use crate::core::Address;
    use crate::metadata::MetadataMap;
    use crate::rt::default_runtime;

    fn stub_lb_config(name: &str) -> GracefulSwitchLbConfig {
        let builder = GLOBAL_LB_REGISTRY.get_policy(name).unwrap();
        GracefulSwitchLbConfig::new(builder, Arc::new(()))
    }

    struct TestSubchannelList {
        subchannels: Vec<Arc<dyn Subchannel>>,
    }

    impl TestSubchannelList {
        fn new(
            addresses: &Vec<Address>,
            channel_controller: &mut dyn ChannelController,
            work_scheduler: Arc<dyn WorkScheduler>,
        ) -> Self {
            let mut scl = TestSubchannelList {
                subchannels: Vec::new(),
            };
            for address in addresses {
                let (sc, _state) =
                    channel_controller.new_subchannel(address, work_scheduler.clone());
                scl.subchannels.push(sc.clone());
            }
            scl
        }

        fn contains(&self, sc: &Arc<dyn Subchannel>) -> bool {
            self.subchannels.contains(sc)
        }
    }

    #[derive(Debug)]
    struct TestPicker {
        name: &'static str,
    }

    impl TestPicker {
        fn new(name: &'static str) -> Self {
            Self { name }
        }
    }
    impl Picker for TestPicker {
        fn pick(&self, _req: &RequestHeaders) -> PickResult {
            PickResult::Pick(Pick {
                subchannel: Arc::new(TestSubchannel::new(
                    Address {
                        address: self.name.to_string().into(),
                        ..Default::default()
                    },
                    mpsc::channel().0,
                )),
                metadata: MetadataMap::new(),
                on_complete: None,
            })
        }
    }

    struct TestState {
        subchannel_list: TestSubchannelList,
    }

    // Defines the functions resolver_update and work to test
    // graceful switch.
    fn create_funcs_for_gracefulswitch_tests(name: &'static str) -> StubPolicyFuncs {
        StubPolicyFuncs {
            // Closure for resolver_update. It creates a subchannel for the
            // endpoint it receives and stores which endpoint it received and
            // which subchannel this child created in the data field.
            resolver_update: Some(Arc::new(
                move |data: &mut StubPolicyData, update: ResolverUpdate, _, channel_controller| {
                    if let Ok(ref endpoints) = update.endpoints {
                        let addresses: Vec<_> = endpoints
                            .iter()
                            .flat_map(|ep| ep.addresses.clone())
                            .collect();
                        let scl = TestSubchannelList::new(
                            &addresses,
                            channel_controller,
                            data.lb_policy_options.work_scheduler.clone(),
                        );
                        let child_state = TestState {
                            subchannel_list: scl,
                        };
                        data.test_data = Some(Box::new(child_state));
                    } else {
                        data.test_data = None;
                    }
                    Ok(())
                },
            )),
            // Closure for work. Verify that the subchannel being updated now is
            // the same one that this child policy created in resolver_update.
            // It then sends a picker of the same state that was passed to it.
            work: Some(Arc::new(
                move |data: &mut StubPolicyData, work_data, channel_controller| {
                    let update = work_data
                        .expect("expected work data")
                        .downcast::<SubchannelUpdate>()
                        .expect("expected SubchannelUpdate");
                    // Retrieve the specific TestState from the generic test_data field.
                    // This downcasts the `Any` trait object.
                    let test_data = data.test_data.as_mut().unwrap();
                    let test_state = test_data.downcast_mut::<TestState>().unwrap();
                    let scl = &mut test_state.subchannel_list;
                    assert!(
                        scl.contains(&update.subchannel),
                        "work received an update for a subchannel it does not own."
                    );
                    channel_controller.update_picker(LbState {
                        connectivity_state: update.state.connectivity_state,
                        picker: Arc::new(TestPicker { name }),
                    });
                },
            )),
            ..Default::default()
        }
    }

    // Constructs the test environment for GracefulSwitchPolicy tests.
    fn new_env() -> TestEnv<GracefulSwitchPolicy> {
        TestEnv::new(|work_scheduler| GracefulSwitchPolicy::new(default_runtime(), work_scheduler))
    }

    impl TestEnv<GracefulSwitchPolicy> {
        // Verifies that the policy produced a new picker that picks a
        // subchannel whose address is `name`.
        fn verify_correct_picker(&mut self, name: &str) {
            println!("verify ready picker");
            let update = self.expect_picker_update();
            let req = test_utils::new_request_headers();
            println!("{:?}", update.connectivity_state);

            let pick = update.picker.pick(&req);
            let PickResult::Pick(pick) = pick else {
                panic!("unexpected pick result: {:?}", pick);
            };
            let received_address = &pick.subchannel.address().address.to_string();
            assert_eq!(received_address, name);
        }
    }

    fn create_endpoint_with_one_address(addr: String) -> Endpoint {
        Endpoint {
            addresses: vec![Address {
                address: addr.into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    // Tests that the gracefulswitch policy correctly sets a child and sends
    // updates to that child when it receives its first config.
    #[test]
    fn gracefulswitch_successful_first_update() {
        reg_stub_policy(
            "stub-gracefulswitch_successful_first_update-one",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_successful_first_update-one",
            ),
        );
        reg_stub_policy(
            "stub-gracefulswitch_successful_first_update-two",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_successful_first_update-two",
            ),
        );

        let mut env = new_env();
        let parsed_config = stub_lb_config("stub-gracefulswitch_successful_first_update-one");

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        let subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&subchannel, &SubchannelState::ready());
        env.verify_correct_picker("stub-gracefulswitch_successful_first_update-one");
    }

    // Tests that the gracefulswitch policy correctly sets a pending child and
    // sends subchannel updates to that child when it receives a new config.
    #[test]
    fn gracefulswitch_switching_to_resolver_update() {
        let mut env = new_env();
        reg_stub_policy(
            "stub-gracefulswitch_switching_to_resolver_update-one",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_switching_to_resolver_update-one",
            ),
        );
        reg_stub_policy(
            "stub-gracefulswitch_switching_to_resolver_update-two",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_switching_to_resolver_update-two",
            ),
        );

        let parsed_config = stub_lb_config("stub-gracefulswitch_switching_to_resolver_update-one");

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        // Subchannel creation and ready
        let subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&subchannel, &SubchannelState::ready());

        // Assert picker is TestPickerOne by checking subchannel address
        env.verify_correct_picker("stub-gracefulswitch_switching_to_resolver_update-one");

        // 2. Switch to mock_policy_two as pending
        let new_parsed_config =
            stub_lb_config("stub-gracefulswitch_switching_to_resolver_update-two");
        env.policy
            .resolver_update(update.clone(), &new_parsed_config, &mut env.tcc)
            .unwrap();

        // Simulate subchannel creation and ready for pending
        let subchannel_two = env.expect_new_subchannel();
        env.send_subchannel_update(&subchannel_two, &SubchannelState::ready());
        // Assert picker is TestPickerTwo by checking subchannel address
        env.verify_correct_picker("stub-gracefulswitch_switching_to_resolver_update-two");
        env.expect_no_events();
    }

    // Tests that the gracefulswitch policy should do nothing when it receives a
    // new config of the same policy that it received before.
    #[test]
    fn gracefulswitch_two_policies_same_type() {
        let mut env = new_env();
        reg_stub_policy(
            "stub-gracefulswitch_two_policies_same_type-one",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_two_policies_same_type-one"),
        );
        let parsed_config = stub_lb_config("stub-gracefulswitch_two_policies_same_type-one");
        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();
        let subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&subchannel, &SubchannelState::ready());
        env.verify_correct_picker("stub-gracefulswitch_two_policies_same_type-one");

        let parsed_config2 = stub_lb_config("stub-gracefulswitch_two_policies_same_type-one");
        env.policy
            .resolver_update(update.clone(), &parsed_config2, &mut env.tcc)
            .unwrap();
        let subchannel = env.expect_new_subchannel();
        assert_eq!(&*subchannel.address().address, "127.0.0.1:1234");
        env.expect_no_events();
    }

    // Tests that the gracefulswitch policy should replace the current child
    // with the pending child if the current child isn't ready.
    #[test]
    fn gracefulswitch_current_not_ready_pending_update() {
        let mut env = new_env();
        reg_stub_policy(
            "stub-gracefulswitch_current_not_ready_pending_update-one",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_current_not_ready_pending_update-one",
            ),
        );
        reg_stub_policy(
            "stub-gracefulswitch_current_not_ready_pending_update-two",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_current_not_ready_pending_update-two",
            ),
        );

        let parsed_config =
            stub_lb_config("stub-gracefulswitch_current_not_ready_pending_update-one");

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let second_endpoint = create_endpoint_with_one_address("0.0.0.0.0".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        env.expect_new_subchannel();
        env.expect_no_events();

        let second_update = ResolverUpdate {
            endpoints: Ok(vec![second_endpoint.clone()]),
            ..Default::default()
        };
        let new_parsed_config =
            stub_lb_config("stub-gracefulswitch_current_not_ready_pending_update-two");
        env.policy
            .resolver_update(second_update.clone(), &new_parsed_config, &mut env.tcc)
            .unwrap();

        let second_subchannel = env.expect_new_subchannel();
        env.expect_no_events();

        env.send_subchannel_update(&second_subchannel, &SubchannelState::ready());
        env.verify_correct_picker("stub-gracefulswitch_current_not_ready_pending_update-two");
        env.expect_no_events();
    }

    // Tests that the gracefulswitch policy should replace the current child
    // with the pending child if the current child was ready but then leaves ready.
    #[test]
    fn gracefulswitch_current_leaving_ready() {
        let mut env = new_env();
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-one",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-one"),
        );
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-two",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-two"),
        );
        let parsed_config = stub_lb_config("stub-gracefulswitch_current_leaving_ready-one");

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let endpoint2 = create_endpoint_with_one_address("127.0.0.1:1235".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        let current_subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&current_subchannel, &SubchannelState::ready());
        env.verify_correct_picker("stub-gracefulswitch_current_leaving_ready-one");
        let new_update = ResolverUpdate {
            endpoints: Ok(vec![endpoint2.clone()]),
            ..Default::default()
        };
        let new_parsed_config = stub_lb_config("stub-gracefulswitch_current_leaving_ready-two");
        env.policy
            .resolver_update(new_update.clone(), &new_parsed_config, &mut env.tcc)
            .unwrap();

        let pending_subchannel = env.expect_new_subchannel();

        env.send_subchannel_update(&pending_subchannel, &SubchannelState::connecting());
        // This should not produce an update.
        env.expect_no_events();
        env.send_subchannel_update(&current_subchannel, &SubchannelState::connecting());
        env.verify_correct_picker("stub-gracefulswitch_current_leaving_ready-two");
    }

    // Tests that the gracefulswitch policy should replace the current child
    // with the pending child if the pending child leaves connecting.
    #[test]
    fn gracefulswitch_pending_leaving_connecting() {
        let mut env = new_env();
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-one",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-one"),
        );
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-two",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-two"),
        );
        let parsed_config = stub_lb_config("stub-gracefulswitch_current_leaving_ready-one");
        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let endpoint2 = create_endpoint_with_one_address("127.0.0.1:1235".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        let current_subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&current_subchannel, &SubchannelState::ready());
        env.verify_correct_picker("stub-gracefulswitch_current_leaving_ready-one");
        let new_update = ResolverUpdate {
            endpoints: Ok(vec![endpoint2.clone()]),
            ..Default::default()
        };
        let new_parsed_config = stub_lb_config("stub-gracefulswitch_current_leaving_ready-two");

        env.policy
            .resolver_update(new_update.clone(), &new_parsed_config, &mut env.tcc)
            .unwrap();

        let pending_subchannel = env.expect_new_subchannel();

        env.send_subchannel_update(
            &pending_subchannel,
            &SubchannelState::transient_failure("n/a"),
        );
        env.verify_correct_picker("stub-gracefulswitch_current_leaving_ready-two");
        env.send_subchannel_update(&pending_subchannel, &SubchannelState::connecting());
        env.verify_correct_picker("stub-gracefulswitch_current_leaving_ready-two");
    }

    // Tests that the gracefulswitch policy should remove the current child's
    // subchannels after swapping.
    #[test]
    fn gracefulswitch_subchannels_removed_after_current_child_swapped() {
        let mut env = new_env();
        reg_stub_policy(
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one",
            ),
        );
        reg_stub_policy(
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two",
            create_funcs_for_gracefulswitch_tests(
                "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two",
            ),
        );
        let parsed_config = stub_lb_config(
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one",
        );
        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        let current_subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&current_subchannel, &SubchannelState::ready());
        env.verify_correct_picker(
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one",
        );
        let second_endpoint = create_endpoint_with_one_address("127.0.0.1:1235".to_string());
        let second_update = ResolverUpdate {
            endpoints: Ok(vec![second_endpoint.clone()]),
            ..Default::default()
        };
        let new_parsed_config = stub_lb_config(
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two",
        );
        env.policy
            .resolver_update(second_update.clone(), &new_parsed_config, &mut env.tcc)
            .unwrap();
        let pending_subchannel = env.expect_new_subchannel();
        println!("moving subchannel to idle");
        env.send_subchannel_update(&pending_subchannel, &SubchannelState::idle());
        env.verify_correct_picker(
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two",
        );
        assert!(Arc::strong_count(&current_subchannel) == 1);
    }
}
