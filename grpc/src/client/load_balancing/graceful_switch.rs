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
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::child_manager::Child;
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
/// always either one or two child policies, once the first resolver update is
/// received.  When there is one policy, all operations are delegated to it.
/// When the child policy type needs to change, graceful switch creates a
/// "pending" child policy alongside the "active" policy.  When the pending
/// policy leaves the CONNECTING state, or when the active policy is not READY,
/// graceful switch will promote the pending policy to active and tear down the
/// previously active policy.
#[derive(Debug)]
pub struct GracefulSwitchPolicy {
    /// None until the first resolver update is received.
    active: Option<Child>,
    pending: Option<Child>,
    runtime: GrpcRuntime,
    work_scheduler: Arc<dyn WorkScheduler>,
}

impl LbPolicy for GracefulSwitchPolicy {
    type LbConfig = GracefulSwitchLbConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let new_name = config.child_builder.name();
        match &mut self.active {
            // The first config always becomes the active policy.
            None => {
                self.active = Some(self.create_child(&config.child_builder));
            }
            // The config names the active policy; abandon any pending policy.
            Some(active) if active.name() == new_name => {
                active.set_suppress_resolution(false);
                self.pending = None;
            }
            // The config names the pending policy; reuse it.
            Some(_) if self.pending.as_ref().is_some_and(|p| p.name() == new_name) => {}
            // The config names a new policy, which becomes the pending policy.
            // The active policy no longer receives resolver updates, so it must
            // not be able to request new ones.
            Some(active) => {
                active.set_suppress_resolution(true);
                self.pending = Some(self.create_child(&config.child_builder));
            }
        }

        // The update always goes to the most recent policy.
        let child = self.latest_child().unwrap();
        let result = child.resolver_update(update, &config.child_config, channel_controller);
        self.reconcile(channel_controller);
        result
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        let Some(data) = data else {
            debug_assert!(false, "GracefulSwitch::work called with None value");
            return;
        };

        // Try sending the work item to the pending child if there is one.
        let data = match &mut self.pending {
            None => data,
            Some(pending) => {
                match pending.try_work(data, channel_controller) {
                    // Work item was consumed; reconcile children and return.
                    Ok(()) => return self.reconcile(channel_controller),
                    // Work item was not consumed; keep trying.
                    Err(data) => data,
                }
            }
        };

        // Now try sending it to the active policy, or drop it.
        let Some(active) = &mut self.active else {
            debug_assert!(false, "work called before resolver_update");
            return;
        };
        let _ = active.try_work(data, channel_controller);
        self.reconcile(channel_controller);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        // Only the most recent policy is asked to exit idle.
        let Some(child) = self.latest_child() else {
            return;
        };
        child.exit_idle(channel_controller);
        self.reconcile(channel_controller);
    }
}

impl GracefulSwitchPolicy {
    /// Creates a new Graceful Switch policy.
    pub fn new(runtime: GrpcRuntime, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        GracefulSwitchPolicy {
            active: None,
            pending: None,
            runtime,
            work_scheduler,
        }
    }

    /// Returns the most recently created child policy (pending if present,
    /// otherwise active).
    fn latest_child(&mut self) -> Option<&mut Child> {
        self.pending.as_mut().or(self.active.as_mut())
    }

    /// Constructs a new child policy and returns it.
    fn create_child(&self, builder: &DynLbPolicyBuilder) -> Child {
        let options = LbPolicyOptions {
            work_scheduler: self.work_scheduler.clone(),
            runtime: self.runtime.clone(),
        };
        Child::new(builder, options)
    }

    /// Called after every call into a child.
    ///
    /// Promotes pending to active and/or reports a picker update to the channel
    /// as appropriate.
    fn reconcile(&mut self, channel_controller: &mut dyn ChannelController) {
        // Both flags should be cleared, so avoid short-circuiting.
        let pending_updated = self.pending.as_mut().is_some_and(Child::take_updated);
        let active_updated = self.active.as_mut().is_some_and(Child::take_updated);
        if !pending_updated && !active_updated {
            return;
        }

        // The pending policy is promoted once it has left CONNECTING, or as
        // soon as the active policy stops being READY.
        let swap = self.pending.as_ref().is_some_and(|pending| {
            pending.state().connectivity_state != ConnectivityState::Connecting
                || self.active.as_ref().unwrap().state().connectivity_state
                    != ConnectivityState::Ready
        });
        if swap {
            self.active = self.pending.take();
        }

        // If we swapped then we need to produce the previously-pending policy's
        // picker.  Or if we did not swap, but the active policy updated itself,
        // we should forward its update.
        if swap || active_updated {
            let active = self.active.as_ref().unwrap();
            channel_controller.update_picker(active.state().clone());
        }
    }
}

#[cfg(test)]
mod test {
    use std::panic;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::mpsc;

    use crate::call_attributes::CallAttributes;
    use crate::client::load_balancing::ChannelController;
    use crate::client::load_balancing::GLOBAL_LB_REGISTRY;
    use crate::client::load_balancing::LbPolicy;
    use crate::client::load_balancing::LbState;
    use crate::client::load_balancing::Pick;
    use crate::client::load_balancing::PickOptions;
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
        fn pick(&self, _options: PickOptions<'_>) -> PickResult {
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

            let pick = update
                .picker
                .pick(PickOptions::new(&req, &CallAttributes::new()));
            let PickResult::Pick(pick) = pick else {
                panic!("unexpected pick result: {:?}", pick);
            };
            let received_address = &pick.subchannel.address().address.to_string();
            assert_eq!(received_address, name);
        }
    }

    // Returns a resolver update containing a single endpoint with one address.
    fn update_with_address(addr: &str) -> ResolverUpdate {
        ResolverUpdate {
            endpoints: Ok(vec![Endpoint {
                addresses: vec![Address {
                    address: addr.to_string().into(),
                    ..Default::default()
                }],
                ..Default::default()
            }]),
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

        let update = update_with_address("127.0.0.1:1234");
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

        let update = update_with_address("127.0.0.1:1234");

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
        let update = update_with_address("127.0.0.1:1234");
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

        let update = update_with_address("127.0.0.1:1234");

        // Switch to first one (current)
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        env.expect_new_subchannel();
        env.expect_no_events();

        let second_update = update_with_address("0.0.0.0.0");
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

        let update = update_with_address("127.0.0.1:1234");

        // Switch to first one (current)
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        let current_subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&current_subchannel, &SubchannelState::ready());
        env.verify_correct_picker("stub-gracefulswitch_current_leaving_ready-one");
        let new_update = update_with_address("127.0.0.1:1235");
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
        let update = update_with_address("127.0.0.1:1234");

        // Switch to first one (current)
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        let current_subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&current_subchannel, &SubchannelState::ready());
        env.verify_correct_picker("stub-gracefulswitch_current_leaving_ready-one");
        let new_update = update_with_address("127.0.0.1:1235");
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
        let update = update_with_address("127.0.0.1:1234");
        env.policy
            .resolver_update(update.clone(), &parsed_config, &mut env.tcc)
            .unwrap();

        let current_subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&current_subchannel, &SubchannelState::ready());
        env.verify_correct_picker(
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one",
        );
        let second_update = update_with_address("127.0.0.1:1235");
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

    // Defines stub functions that create a subchannel per address in
    // resolver_update, and request re-resolution before producing a picker in
    // work.
    fn create_funcs_requesting_resolution(name: &'static str) -> StubPolicyFuncs {
        StubPolicyFuncs {
            resolver_update: Some(Arc::new(
                move |data: &mut StubPolicyData, update: ResolverUpdate, _, channel_controller| {
                    let addresses: Vec<_> = update
                        .endpoints
                        .unwrap()
                        .iter()
                        .flat_map(|ep| ep.addresses.clone())
                        .collect();
                    TestSubchannelList::new(
                        &addresses,
                        channel_controller,
                        data.lb_policy_options.work_scheduler.clone(),
                    );
                    Ok(())
                },
            )),
            work: Some(Arc::new(move |_data, work_data, channel_controller| {
                let update = work_data
                    .expect("expected work data")
                    .downcast::<SubchannelUpdate>()
                    .expect("expected SubchannelUpdate");
                channel_controller.request_resolution();
                channel_controller.update_picker(LbState {
                    connectivity_state: update.state.connectivity_state,
                    picker: Arc::new(TestPicker { name }),
                });
            })),
            ..Default::default()
        }
    }

    // Tests that re-resolution requests from the active child are dropped while
    // a pending child exists, since the active child no longer receives
    // resolver updates.  Requests from the child whose updates are being used
    // are forwarded to the channel.
    #[test]
    fn gracefulswitch_active_resolution_request_ignored_with_pending() {
        let name_one = "stub-gracefulswitch_active_resolution_request-one";
        let name_two = "stub-gracefulswitch_active_resolution_request-two";
        reg_stub_policy(name_one, create_funcs_requesting_resolution(name_one));
        reg_stub_policy(name_two, create_funcs_requesting_resolution(name_two));

        let mut env = new_env();
        let update = update_with_address("127.0.0.1:1234");
        env.policy
            .resolver_update(update.clone(), &stub_lb_config(name_one), &mut env.tcc)
            .unwrap();
        let active_subchannel = env.expect_new_subchannel();

        // With no pending child, the active child's request is forwarded.
        env.send_subchannel_update(&active_subchannel, &SubchannelState::ready());
        env.expect_request_resolution();
        env.verify_correct_picker(name_one);

        // Create a pending child.
        let second_update = update_with_address("127.0.0.1:1235");
        env.policy
            .resolver_update(second_update, &stub_lb_config(name_two), &mut env.tcc)
            .unwrap();
        let pending_subchannel = env.expect_new_subchannel();
        env.expect_no_events();

        // The active child's request is dropped now that a pending child
        // exists.  Its picker is still used, as it remains READY.
        env.send_subchannel_update(&active_subchannel, &SubchannelState::ready());
        env.verify_correct_picker(name_one);
        env.expect_no_events();

        // The pending child's requests are forwarded.
        env.send_subchannel_update(&pending_subchannel, &SubchannelState::ready());
        env.expect_request_resolution();
        env.verify_correct_picker(name_two);
        env.expect_no_events();
    }

    // Defines stub functions that record the name of the policy whenever its
    // exit_idle method is called.
    fn create_funcs_recording_exit_idle(
        name: &'static str,
        exit_idle_calls: Arc<Mutex<Vec<&'static str>>>,
    ) -> StubPolicyFuncs {
        StubPolicyFuncs {
            exit_idle: Some(Arc::new(move |_data, _channel_controller| {
                exit_idle_calls.lock().unwrap().push(name);
            })),
            ..Default::default()
        }
    }

    // Tests that exit_idle is delivered to the pending child if one exists, and
    // to the active child otherwise.  The pending child is the one whose picker
    // will be used once it connects, so it is the one that needs to wake up.
    #[test]
    fn gracefulswitch_exit_idle_wakes_latest_child() {
        let name_one = "stub-gracefulswitch_exit_idle_wakes_latest_child-one";
        let name_two = "stub-gracefulswitch_exit_idle_wakes_latest_child-two";
        let calls: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        reg_stub_policy(
            name_one,
            create_funcs_recording_exit_idle(name_one, calls.clone()),
        );
        reg_stub_policy(
            name_two,
            create_funcs_recording_exit_idle(name_two, calls.clone()),
        );

        let mut env = new_env();
        let update = update_with_address("127.0.0.1:1234");

        // Before any config is received, exit_idle does nothing.
        env.policy.exit_idle(&mut env.tcc);
        assert!(calls.lock().unwrap().is_empty());

        // With only an active child, the active child is woken.
        env.policy
            .resolver_update(update.clone(), &stub_lb_config(name_one), &mut env.tcc)
            .unwrap();
        env.policy.exit_idle(&mut env.tcc);
        assert_eq!(*calls.lock().unwrap(), vec![name_one]);
        calls.lock().unwrap().clear();

        // With a pending child, only the pending child is woken.
        env.policy
            .resolver_update(update, &stub_lb_config(name_two), &mut env.tcc)
            .unwrap();
        env.policy.exit_idle(&mut env.tcc);
        assert_eq!(*calls.lock().unwrap(), vec![name_two]);

        env.expect_no_events();
    }

    // Tests that the first config received becomes the active child
    // immediately, so that a second config of a different type becomes a
    // pending child instead of replacing the first one.
    #[test]
    fn gracefulswitch_first_config_becomes_active() {
        let name_one = "stub-gracefulswitch_first_config_becomes_active-one";
        let name_two = "stub-gracefulswitch_first_config_becomes_active-two";
        reg_stub_policy(name_one, create_funcs_for_gracefulswitch_tests(name_one));
        reg_stub_policy(name_two, create_funcs_for_gracefulswitch_tests(name_two));

        let mut env = new_env();
        let update = update_with_address("127.0.0.1:1234");
        env.policy
            .resolver_update(update.clone(), &stub_lb_config(name_one), &mut env.tcc)
            .unwrap();
        let active_subchannel = env.expect_new_subchannel();

        // The first child has not produced a picker yet, but it is already the
        // active child, so this config becomes a pending child.
        let second_update = update_with_address("127.0.0.1:1235");
        env.policy
            .resolver_update(second_update, &stub_lb_config(name_two), &mut env.tcc)
            .unwrap();
        let pending_subchannel = env.expect_new_subchannel();
        env.expect_no_events();

        // The first child still exists and is still the active child, so its
        // picker is used when it becomes READY.
        env.send_subchannel_update(&active_subchannel, &SubchannelState::ready());
        env.verify_correct_picker(name_one);
        env.expect_no_events();

        // Once the pending child is READY, it is swapped in.
        env.send_subchannel_update(&pending_subchannel, &SubchannelState::ready());
        env.verify_correct_picker(name_two);
        env.expect_no_events();
    }

    // Tests that a config naming the active policy abandons an existing pending
    // child, after which updates for the pending child's subchannels are
    // ignored.
    #[test]
    fn gracefulswitch_same_type_config_drops_pending() {
        let name_one = "stub-gracefulswitch_same_type_config_drops_pending-one";
        let name_two = "stub-gracefulswitch_same_type_config_drops_pending-two";
        reg_stub_policy(name_one, create_funcs_for_gracefulswitch_tests(name_one));
        reg_stub_policy(name_two, create_funcs_for_gracefulswitch_tests(name_two));

        let mut env = new_env();
        let update = update_with_address("127.0.0.1:1234");
        env.policy
            .resolver_update(update.clone(), &stub_lb_config(name_one), &mut env.tcc)
            .unwrap();
        let active_subchannel = env.expect_new_subchannel();
        env.send_subchannel_update(&active_subchannel, &SubchannelState::ready());
        env.verify_correct_picker(name_one);

        // Create a pending child.
        let second_update = update_with_address("127.0.0.1:1235");
        env.policy
            .resolver_update(second_update, &stub_lb_config(name_two), &mut env.tcc)
            .unwrap();
        let pending_subchannel = env.expect_new_subchannel();
        env.expect_no_events();

        // Switch back to the active policy's type, which drops the pending
        // child.  The active child receives the update.
        env.policy
            .resolver_update(update, &stub_lb_config(name_one), &mut env.tcc)
            .unwrap();
        let active_subchannel = env.expect_new_subchannel();
        env.expect_no_events();

        // Updates for the dropped child's subchannel are ignored, even one
        // that would otherwise have triggered a swap.
        env.send_subchannel_update(&pending_subchannel, &SubchannelState::ready());
        env.expect_no_events();

        // The active child continues to be used.
        env.send_subchannel_update(&active_subchannel, &SubchannelState::ready());
        env.verify_correct_picker(name_one);
        env.expect_no_events();
    }
}
