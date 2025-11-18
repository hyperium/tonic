use crate::client::load_balancing::child_manager::{ChildManager, ChildUpdate};
use crate::client::load_balancing::{
    ChannelController, LbConfig, LbPolicy, LbPolicyBuilder, LbState, ParsedJsonLbConfig,
    Subchannel, SubchannelState, WorkScheduler, GLOBAL_LB_REGISTRY,
};
use crate::client::name_resolution::ResolverUpdate;
use crate::client::ConnectivityState;
use crate::rt::Runtime;

use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct GracefulSwitchLbConfig {
    child_builder: Arc<dyn LbPolicyBuilder>,
    child_config: Option<LbConfig>,
}

/// A graceful switching load balancing policy.  In graceful switch, there is
/// always either one or two child policies.  When there is one policy, all
/// operations are delegated to it.  When the child policy type needs to change,
/// graceful switch creates a "pending" child policy alongside the "active"
/// policy.  When the pending policy leaves the CONNECTING state, or when the
/// active policy is not READY, graceful switch will promote the pending policy
/// to to active and tear down the previously active policy.
#[derive(Debug)]
pub(crate) struct GracefulSwitchPolicy {
    child_manager: ChildManager<()>, // Child ID is the name of the child policy.
    last_update: Option<LbState>, // Saves the last output LbState to determine if an update is needed.
    active_child_builder: Option<Arc<dyn LbPolicyBuilder>>,
}

impl LbPolicy for GracefulSwitchPolicy {
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let config = config
            .ok_or("graceful switch received no config")?
            .convert_to::<GracefulSwitchLbConfig>()
            .ok_or_else(|| format!("invalid config: {config:?}"))?;

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
            child_update: Some((update, config.child_config.clone())),
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

        let res = self
            .child_manager
            .update(children.into_iter(), channel_controller)?;
        self.update_picker(channel_controller);
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        self.child_manager
            .subchannel_update(subchannel, state, channel_controller);
        self.update_picker(channel_controller);
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        self.child_manager.work(channel_controller);
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
    pub fn new(runtime: Arc<dyn Runtime>, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        GracefulSwitchPolicy {
            child_manager: ChildManager::new(runtime, work_scheduler),
            last_update: None,
            active_child_builder: None,
        }
    }

    /// Parses a child config list and returns a LB config for the
    /// GracefulSwitchPolicy.  Config is expected to contain a JSON array of LB
    /// policy names + configs matching the format of the "loadBalancingConfig"
    /// field in the gRPC ServiceConfig. It returns a type that should be passed
    /// to resolver_update in the LbConfig.config field.
    pub fn parse_config(
        config: &ParsedJsonLbConfig,
    ) -> Result<LbConfig, Box<dyn Error + Send + Sync>> {
        let cfg: Vec<HashMap<String, serde_json::Value>> = match config.convert_to() {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("failed to parse JSON config: {}", e).into());
            }
        };
        for c in cfg {
            if c.len() != 1 {
                return Err(format!(
                    "Each element in array must contain exactly one policy name/config; found {:?}",
                    c.keys()
                )
                .into());
            }
            let (policy_name, policy_config) = c.into_iter().next().unwrap();
            let Some(child_builder) = GLOBAL_LB_REGISTRY.get_policy(policy_name.as_str()) else {
                continue;
            };
            let parsed_config = ParsedJsonLbConfig {
                value: policy_config,
            };
            let child_config = child_builder.parse_config(&parsed_config)?;
            let gsb_config = GracefulSwitchLbConfig {
                child_builder,
                child_config,
            };
            return Ok(LbConfig::new(gsb_config));
        }
        Err("no supported policies found in config".into())
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
            .retain_children([((), pending_child_builder)].into_iter());

        return Some(pending_state);
    }
}

#[cfg(test)]
mod test {
    use crate::client::load_balancing::graceful_switch::GracefulSwitchPolicy;
    use crate::client::load_balancing::test_utils::{
        self, reg_stub_policy, StubPolicyData, StubPolicyFuncs, TestChannelController, TestEvent,
        TestSubchannel, TestWorkScheduler,
    };
    use crate::client::load_balancing::{
        ChannelController, LbPolicy, ParsedJsonLbConfig, PickResult, Picker, Subchannel,
        SubchannelState,
    };
    use crate::client::load_balancing::{LbState, Pick};
    use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
    use crate::client::ConnectivityState;
    use crate::rt::default_runtime;
    use crate::service::Request;
    use std::time::Duration;
    use std::{panic, sync::Arc};
    use tokio::select;
    use tokio::sync::mpsc::{self, UnboundedReceiver};
    use tonic::metadata::MetadataMap;

    const DEFAULT_TEST_SHORT_TIMEOUT: Duration = Duration::from_millis(10);

    struct TestSubchannelList {
        subchannels: Vec<Arc<dyn Subchannel>>,
    }

    impl TestSubchannelList {
        fn new(addresses: &Vec<Address>, channel_controller: &mut dyn ChannelController) -> Self {
            let mut scl = TestSubchannelList {
                subchannels: Vec::new(),
            };
            for address in addresses {
                let sc = channel_controller.new_subchannel(address);
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
        fn pick(&self, _req: &Request) -> PickResult {
            PickResult::Pick(Pick {
                subchannel: Arc::new(TestSubchannel::new(
                    Address {
                        address: self.name.to_string().into(),
                        ..Default::default()
                    },
                    mpsc::unbounded_channel().0,
                )),
                metadata: MetadataMap::new(),
                on_complete: None,
            })
        }
    }

    struct TestState {
        subchannel_list: TestSubchannelList,
    }

    // Defines the functions resolver_update and subchannel_update to test graceful switch
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
                        let scl = TestSubchannelList::new(&addresses, channel_controller);
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
            // Closure for subchannel_update. Verify that the subchannel that
            // being updated now is the same one that this child policy created
            // in resolver_update. It then sends a picker of the same state that
            // was passed to it.
            subchannel_update: Some(Arc::new(
                move |data: &mut StubPolicyData, updated_subchannel, state, channel_controller| {
                    // Retrieve the specific TestState from the generic test_data field.
                    // This downcasts the `Any` trait object.
                    let test_data = data.test_data.as_mut().unwrap();
                    let test_state = test_data.downcast_mut::<TestState>().unwrap();
                    let scl = &mut test_state.subchannel_list;
                    assert!(
                        scl.contains(&updated_subchannel),
                        "subchannel_update received an update for a subchannel it does not own."
                    );
                    channel_controller.update_picker(LbState {
                        connectivity_state: state.connectivity_state,
                        picker: Arc::new(TestPicker { name }),
                    });
                },
            )),
            work: None,
        }
    }

    // Sets up the test environment.
    //
    // Performs the following:
    // 1. Creates a work scheduler.
    // 2. Creates a fake channel that acts as a channel controller.
    // 3. Creates an StubPolicyBuilder with StubFuncs that each test will define
    //    and name of the test.
    // 5. Creates a GracefulSwitch.
    //
    // Returns the following:
    // 1. A receiver for events initiated by the LB policy (like creating a new
    //    subchannel, sending a new picker etc).
    // 2. The GracefulSwitch to send resolver and subchannel updates from the
    //    test.
    // 3. The controller to pass to the LB policy as part of the updates.
    fn setup() -> (
        mpsc::UnboundedReceiver<TestEvent>,
        Box<GracefulSwitchPolicy>,
        Box<dyn ChannelController>,
    ) {
        let (tx_events, rx_events) = mpsc::unbounded_channel::<TestEvent>();
        let work_scheduler = Arc::new(TestWorkScheduler {
            tx_events: tx_events.clone(),
        });

        let tcc = Box::new(TestChannelController {
            tx_events: tx_events.clone(),
        });

        let graceful_switch =
            GracefulSwitchPolicy::new(default_runtime(), Arc::new(TestWorkScheduler { tx_events }));
        (rx_events, Box::new(graceful_switch), tcc)
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

    // Verifies that the next event on rx_events channel is NewSubchannel.
    // Returns the subchannel created.
    async fn verify_subchannel_creation_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
    ) -> Arc<dyn Subchannel> {
        match rx_events.recv().await.unwrap() {
            TestEvent::NewSubchannel(sc) => {
                return sc;
            }
            other => panic!("unexpected event {:?}", other),
        };
    }

    // Verifies that the channel moves to READY state with a picker that returns the
    // given subchannel.
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_correct_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        name: &str,
    ) {
        println!("verify ready picker");
        let event = rx_events.recv().await.unwrap();
        let TestEvent::UpdatePicker(update) = event else {
            panic!("unexpected event {:?}", event);
        };
        let req = test_utils::new_request();
        println!("{:?}", update.connectivity_state);

        let pick = update.picker.pick(&req);
        let PickResult::Pick(pick) = pick else {
            panic!("unexpected pick result: {:?}", pick);
        };
        let received_address = &pick.subchannel.address().address.to_string();
        // It's good practice to create the expected value once.
        let expected_address = name.to_string();

        // Check for inequality and panic with a detailed message if they don't match.
        assert_eq!(received_address, &expected_address);
    }

    fn move_subchannel_to_state(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
        state: ConnectivityState,
    ) {
        lb_policy.subchannel_update(
            subchannel,
            &SubchannelState {
                connectivity_state: state,
                ..Default::default()
            },
            tcc,
        );
    }

    // Tests that the gracefulswitch policy correctly sets a child and sends
    // updates to that child when it receives its first config.
    #[tokio::test]
    async fn gracefulswitch_successful_first_update() {
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

        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
        let service_config = serde_json::json!([
                { "stub-gracefulswitch_successful_first_update-one": serde_json::json!({}) },
                { "stub-gracefulswitch_successful_first_update-two": serde_json::json!({}) }
            ]
        );

        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        let subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannel,
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_successful_first_update-one",
        )
        .await;
    }

    // Tests that the gracefulswitch policy correctly sets a pending child and
    // sends subchannel updates to that child when it receives a new config.
    #[tokio::test]
    async fn gracefulswitch_switching_to_resolver_update() {
        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
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

        let service_config = serde_json::json!([
                { "stub-gracefulswitch_switching_to_resolver_update-one": serde_json::json!({}) }
            ]
        );
        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        // Subchannel creation and ready
        let subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannel,
            tcc.as_mut(),
            ConnectivityState::Ready,
        );

        // Assert picker is TestPickerOne by checking subchannel address
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_switching_to_resolver_update-one",
        )
        .await;

        // 2. Switch to mock_policy_two as pending
        let new_service_config = serde_json::json!([
                { "stub-gracefulswitch_switching_to_resolver_update-two": serde_json::json!({}) }
            ]
        );
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();
        graceful_switch
            .resolver_update(update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();

        // Simulate subchannel creation and ready for pending
        let subchannel_two = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannel_two,
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        // Assert picker is TestPickerTwo by checking subchannel address
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_switching_to_resolver_update-two",
        )
        .await;
        assert_channel_empty(&mut rx_events).await;
    }

    async fn assert_channel_empty(rx_events: &mut UnboundedReceiver<TestEvent>) {
        select! {
            event = rx_events.recv() => {
                panic!("Received unexpected event from policy: {event:?}");
            }
            _ = tokio::time::sleep(DEFAULT_TEST_SHORT_TIMEOUT) => {}
        };
    }

    // Tests that the gracefulswitch policy should do nothing when it receives a
    // new config of the same policy that it received before.
    #[tokio::test]
    async fn gracefulswitch_two_policies_same_type() {
        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
        reg_stub_policy(
            "stub-gracefulswitch_two_policies_same_type-one",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_two_policies_same_type-one"),
        );
        let service_config = serde_json::json!(
            [
                { "stub-gracefulswitch_two_policies_same_type-one": serde_json::json!({}) }
            ]
        );
        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();
        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();
        let subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannel,
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_two_policies_same_type-one",
        )
        .await;

        let service_config2 = serde_json::json!(
            [
                { "stub-gracefulswitch_two_policies_same_type-one": serde_json::json!({}) }
            ]
        );
        let parsed_config2 = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config2,
        })
        .unwrap();
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config2), &mut *tcc)
            .unwrap();
        let subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        assert_eq!(&*subchannel.address().address, "127.0.0.1:1234");
        assert_channel_empty(&mut rx_events).await;
    }

    // Tests that the gracefulswitch policy should replace the current child
    // with the pending child if the current child isn't ready.
    #[tokio::test]
    async fn gracefulswitch_current_not_ready_pending_update() {
        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
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

        let service_config = serde_json::json!([
                { "stub-gracefulswitch_current_not_ready_pending_update-one": serde_json::json!({}) }
            ]
        );

        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let second_endpoint = create_endpoint_with_one_address("0.0.0.0.0".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        let current_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        assert_channel_empty(&mut rx_events).await;

        let new_service_config = serde_json::json!([
                { "stub-gracefulswitch_current_not_ready_pending_update-two": serde_json::json!({ "shuffleAddressList": false }) },
            ]
        );
        let second_update = ResolverUpdate {
            endpoints: Ok(vec![second_endpoint.clone()]),
            ..Default::default()
        };
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();
        graceful_switch
            .resolver_update(second_update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();

        let second_subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        assert_channel_empty(&mut rx_events).await;

        move_subchannel_to_state(
            &mut *graceful_switch,
            second_subchannel,
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_not_ready_pending_update-two",
        )
        .await;
        assert_channel_empty(&mut rx_events).await;
    }

    // Tests that the gracefulswitch policy should replace the current child
    // with the pending child if the current child was ready but then leaves ready.
    #[tokio::test]
    async fn gracefulswitch_current_leaving_ready() {
        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-one",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-one"),
        );
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-two",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-two"),
        );
        let service_config = serde_json::json!([
                { "stub-gracefulswitch_current_leaving_ready-one": serde_json::json!({}) }
            ]
        );
        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let endpoint2 = create_endpoint_with_one_address("127.0.0.1:1235".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        let current_subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannel.clone(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-one",
        )
        .await;
        let new_service_config = serde_json::json!(
            [
                { "stub-gracefulswitch_current_leaving_ready-two": serde_json::json!({}) },

            ]
        );
        let new_update = ResolverUpdate {
            endpoints: Ok(vec![endpoint2.clone()]),
            ..Default::default()
        };
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();
        graceful_switch
            .resolver_update(new_update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();

        let pending_subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;

        move_subchannel_to_state(
            &mut *graceful_switch,
            pending_subchannel,
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );
        // This should not produce an update.
        assert_channel_empty(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannel,
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-two",
        )
        .await;
    }

    // Tests that the gracefulswitch policy should replace the current child
    // with the pending child if the pending child leaves connecting.
    #[tokio::test]
    async fn gracefulswitch_pending_leaving_connecting() {
        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-one",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-one"),
        );
        reg_stub_policy(
            "stub-gracefulswitch_current_leaving_ready-two",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_current_leaving_ready-two"),
        );
        let service_config = serde_json::json!(
            [
                { "stub-gracefulswitch_current_leaving_ready-one": serde_json::json!({}) }
            ]
        );
        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();
        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let endpoint2 = create_endpoint_with_one_address("127.0.0.1:1235".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        let current_subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannel,
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-one",
        )
        .await;
        let new_service_config = serde_json::json!(
            [
                { "stub-gracefulswitch_current_leaving_ready-two": serde_json::json!({}) },
            ]
        );
        let new_update = ResolverUpdate {
            endpoints: Ok(vec![endpoint2.clone()]),
            ..Default::default()
        };
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();

        graceful_switch
            .resolver_update(new_update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();

        let pending_subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;

        move_subchannel_to_state(
            &mut *graceful_switch,
            pending_subchannel.clone(),
            tcc.as_mut(),
            ConnectivityState::TransientFailure,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-two",
        )
        .await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            pending_subchannel,
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-two",
        )
        .await;
    }

    // Tests that the gracefulswitch policy should remove the current child's
    // subchannels after swapping.
    #[tokio::test]
    async fn gracefulswitch_subchannels_removed_after_current_child_swapped() {
        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
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
        let service_config = serde_json::json!(
            [
                { "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one": serde_json::json!({}) }
            ]
        );
        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();
        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        let current_subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannel.clone(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one",
        )
        .await;
        let new_service_config = serde_json::json!(
            [
                { "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two": serde_json::json!({ "shuffleAddressList": false }) },
            ]
        );
        let second_endpoint = create_endpoint_with_one_address("127.0.0.1:1235".to_string());
        let second_update = ResolverUpdate {
            endpoints: Ok(vec![second_endpoint.clone()]),
            ..Default::default()
        };
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();
        graceful_switch
            .resolver_update(second_update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();
        let pending_subchannel = verify_subchannel_creation_from_policy(&mut rx_events).await;
        println!("moving subchannel to idle");
        move_subchannel_to_state(
            &mut *graceful_switch,
            pending_subchannel,
            tcc.as_mut(),
            ConnectivityState::Idle,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two",
        )
        .await;
        assert!(Arc::strong_count(&current_subchannel) == 1);
    }
}
