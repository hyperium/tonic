use crate::client::channel::{InternalChannelController, WorkQueueItem, WorkQueueTx};
use crate::client::load_balancing::{
    ChannelController, ExternalSubchannel, Failing, LbConfig, LbPolicy, LbPolicyBuilder,
    LbPolicyOptions, LbState, ParsedJsonLbConfig, Pick, PickResult, Picker, QueuingPicker,
    Subchannel, SubchannelState, WeakSubchannel, WorkScheduler, GLOBAL_LB_REGISTRY,
};
use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
use crate::client::transport::{Transport, GLOBAL_TRANSPORT_REGISTRY};
use crate::client::ConnectivityState;
use crate::rt::{default_runtime, Runtime};

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::hash::Hash;
use std::mem;
use std::ops::Add;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::service::{Message, Request, Response, Service};
use core::panic;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, Notify};
use tonic::{async_trait, metadata::MetadataMap};

#[derive(Deserialize)]
struct GracefulSwitchConfig {
    children_policies: Vec<HashMap<String, serde_json::Value>>,
}

struct GracefulSwitchLbConfig {
    child_builder: Arc<dyn LbPolicyBuilder>,
    child_config: Option<LbConfig>,
}

impl GracefulSwitchLbConfig {
    fn new(child_builder: Arc<dyn LbPolicyBuilder>, child_config: Option<LbConfig>) -> Self {
        GracefulSwitchLbConfig {
            child_builder,
            child_config,
        }
    }
}

/**
Struct for Graceful Switch.
*/
pub struct GracefulSwitchPolicy {
    subchannel_to_policy: HashMap<WeakSubchannel, ChildKind>,
    managing_policy: Mutex<ChildPolicyManager>,
    work_scheduler: Arc<dyn WorkScheduler>,
    runtime: Arc<dyn Runtime>,
}

impl LbPolicy for GracefulSwitchPolicy {
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if update.service_config.as_ref().is_ok_and(|sc| sc.is_some()) {
            return Err("can't do service configs yet".into());
        }
        let cfg: Arc<GracefulSwitchLbConfig> =
            match config.unwrap().convert_to::<Arc<GracefulSwitchLbConfig>>() {
                Ok(cfg) => (*cfg).clone(),
                Err(e) => panic!("convert_to failed: {e}"),
            };
        let mut wrapped_channel_controller = WrappedController::new(channel_controller);
        let mut target_child_kind = ChildKind::Pending;

        // Determine if we can switch the new policy in. If there is no children
        // yet or the new policy isn't the same as the latest policy, then
        // we can swap.
        let needs_switch = {
            let mut managing_policy = self.managing_policy.lock().unwrap();
            managing_policy.no_policy()
                || managing_policy.latest_policy() != cfg.child_builder.name()
        };

        if needs_switch {
            target_child_kind = self.switch_to(config);
        }
        {
            let mut managing_policy = self.managing_policy.lock().unwrap();
            if let Some(child_policy) = managing_policy.get_child_policy(&target_child_kind) {
                child_policy.policy.resolver_update(
                    update,
                    cfg.child_config.as_ref(),
                    &mut wrapped_channel_controller,
                )?;
            }
        }
        self.resolve_child_controller(&mut wrapped_channel_controller, target_child_kind);
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        let mut wrapped_channel_controller = WrappedController::new(channel_controller);
        let which_child = self
            .subchannel_to_policy
            .get(&WeakSubchannel::new(&subchannel))
            .unwrap_or_else(|| {
                panic!("Subchannel not found in graceful switch: {}", subchannel);
            });
        {
            let mut managing_policy = self.managing_policy.lock().unwrap();
            if let Some(child_policy) = managing_policy.get_child_policy(which_child) {
                child_policy.policy.subchannel_update(
                    subchannel,
                    state,
                    &mut wrapped_channel_controller,
                );
            }
        }
        self.resolve_child_controller(&mut wrapped_channel_controller, which_child.clone());
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        let mut wrapped_channel_controller = WrappedController::new(channel_controller);
        let mut child_kind = ChildKind::Pending;
        {
            let mut managing_policy = self.managing_policy.lock().unwrap();
            if let Some(ref mut pending_child) = managing_policy.pending_child {
                pending_child.policy.work(&mut wrapped_channel_controller);
            } else if let Some(ref mut current_child) = managing_policy.current_child {
                current_child.policy.work(&mut wrapped_channel_controller);
                child_kind = ChildKind::Current;
            }
        }
        self.resolve_child_controller(&mut wrapped_channel_controller, child_kind);
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        let mut wrapped_channel_controller = WrappedController::new(channel_controller);
        let mut child_kind = ChildKind::Pending;
        {
            let mut managing_policy = self.managing_policy.lock().unwrap();
            if let Some(ref mut pending_child) = managing_policy.pending_child {
                pending_child
                    .policy
                    .exit_idle(&mut wrapped_channel_controller);
            } else if let Some(ref mut current_child) = managing_policy.current_child {
                current_child
                    .policy
                    .exit_idle(&mut wrapped_channel_controller);
                child_kind = ChildKind::Current;
            }
        }
        self.resolve_child_controller(&mut wrapped_channel_controller, child_kind);
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum ChildKind {
    Current,
    Pending,
}

impl GracefulSwitchPolicy {
    /// Create a new Graceful Switch policy.
    pub fn new(work_scheduler: Arc<dyn WorkScheduler>, runtime: Arc<dyn Runtime>) -> Self {
        GracefulSwitchPolicy {
            subchannel_to_policy: HashMap::default(),
            managing_policy: Mutex::new(ChildPolicyManager::new()),
            work_scheduler,
            runtime,
        }
    }

    fn resolve_child_controller(
        &mut self,
        channel_controller: &mut WrappedController,
        child_kind: ChildKind,
    ) {
        let mut should_swap = false;
        let mut final_child_kind = child_kind.clone();
        {
            let mut managing_policy = self.managing_policy.lock().unwrap();

            match child_kind {
                ChildKind::Pending => {
                    if let Some(ref mut pending_policy) = managing_policy.pending_child {
                        if let Some(picker) = channel_controller.picker_update.take() {
                            pending_policy.policy_state = picker.connectivity_state;
                            pending_policy.policy_picker_update = Some(picker);
                        }
                    }
                }

                ChildKind::Current => {
                    if let Some(ref mut current_policy) = managing_policy.current_child {
                        if let Some(picker) = channel_controller.picker_update.take() {
                            current_policy.policy_state = picker.connectivity_state;
                            channel_controller.channel_controller.update_picker(picker);
                        }
                    }
                }
            }

            let current_child = managing_policy.current_child.as_ref();
            let pending_child = managing_policy.pending_child.as_ref();

            // If the current child is in any state but Ready and the pending
            // child is in any state but connecting, then the policies should
            // swap.
            if let (Some(current_child), Some(pending_child)) = (current_child, pending_child) {
                if current_child.policy_state != ConnectivityState::Ready
                    || pending_child.policy_state != ConnectivityState::Connecting
                {
                    println!("Condition met, should swap.");
                    should_swap = true;
                }
            }
        }

        if should_swap {
            self.swap(channel_controller);
            final_child_kind = ChildKind::Current;
        }

        // Any created subchannels are mapped to the appropriate child.
        for csc in &channel_controller.created_subchannels {
            println!("Printing csc: {:?}", csc);
            let key = WeakSubchannel::new(csc);
            self.subchannel_to_policy
                .entry(key)
                .or_insert_with(|| final_child_kind.clone());
        }
    }

    fn swap(&mut self, channel_controller: &mut WrappedController) {
        let mut managing_policy = self.managing_policy.lock().unwrap();
        managing_policy.current_child = managing_policy.pending_child.take();
        self.subchannel_to_policy
            .retain(|_, v| *v == ChildKind::Pending);

        // Remap all the subchannels mapped to Pending to Current.
        for (_, child_kind) in self.subchannel_to_policy.iter_mut() {
            if *child_kind == ChildKind::Pending {
                *child_kind = ChildKind::Current;
            }
        }

        // Send the pending child's cached picker update.
        if let Some(current) = &mut managing_policy.current_child {
            if let Some(picker) = current.policy_picker_update.take() {
                channel_controller.channel_controller.update_picker(picker);
            }
        }
    }

    fn parse_config(config: &ParsedJsonLbConfig) -> Result<LbConfig, Box<dyn Error + Send + Sync>> {
        let cfg: GracefulSwitchConfig = match config.convert_to() {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("failed to parse JSON config: {}", e).into());
            }
        };
        for c in &cfg.children_policies {
            assert!(
                c.len() == 1,
                "Each children_policies entry must contain exactly one policy, found {}",
                c.len()
            );
            if let Some((policy_name, policy_config)) = c.iter().next() {
                if let Some(child) = GLOBAL_LB_REGISTRY.get_policy(policy_name.as_str()) {
                    if policy_name == "round_robin" {
                        println!("is round robin");
                        let graceful_switch_lb_config = GracefulSwitchLbConfig::new(child, None);
                        return Ok(LbConfig::new(Arc::new(graceful_switch_lb_config)));
                    }
                    let parsed_config = ParsedJsonLbConfig {
                        value: policy_config.clone(),
                    };
                    let config_result = child.parse_config(&parsed_config);
                    let config = match config_result {
                        Ok(Some(cfg)) => cfg,
                        Ok(None) => {
                            return Err("child policy config returned None".into());
                        }
                        Err(e) => {
                            println!("returning error in parse_config");
                            return Err(
                                format!("failed to parse child policy config: {}", e).into()
                            );
                        }
                    };
                    let graceful_switch_lb_config =
                        GracefulSwitchLbConfig::new(child, Some(config));
                    return Ok(LbConfig::new(Arc::new(graceful_switch_lb_config)));
                } else {
                    continue;
                }
            } else {
                continue;
            }
        }
        Err("no supported policies found in config".into())
    }

    fn switch_to(&mut self, config: Option<&LbConfig>) -> ChildKind {
        let cfg: Arc<GracefulSwitchLbConfig> =
            match config.unwrap().convert_to::<Arc<GracefulSwitchLbConfig>>() {
                Ok(cfg) => (*cfg).clone(),
                Err(e) => panic!("convert_to failed: {e}"),
            };
        let options = LbPolicyOptions {
            work_scheduler: self.work_scheduler.clone(),
            runtime: self.runtime.clone(),
        };
        let new_policy = cfg.child_builder.build(options);
        let mut managing_policy = self.managing_policy.lock().unwrap();

        let new_child = ChildPolicy::new(
            cfg.child_builder.clone(),
            new_policy,
            ConnectivityState::Connecting,
        );
        if managing_policy.current_child.is_none() {
            managing_policy.current_child = Some(new_child);
            ChildKind::Current
        } else {
            managing_policy.pending_child = Some(new_child);
            ChildKind::Pending
        }
    }
}

// Struct to wrap a channel controller around. The purpose is to
// store a picker update to check connectivity state of a child.
// This helps to decide whether to swap or not in subchannel_update.
// Also tracks created_subchannels, which then is then used to map subchannels to
// children policies.
struct WrappedController<'a> {
    channel_controller: &'a mut dyn ChannelController,
    created_subchannels: Vec<Arc<dyn Subchannel>>,
    picker_update: Option<LbState>,
}

impl<'a> WrappedController<'a> {
    fn new(channel_controller: &'a mut dyn ChannelController) -> Self {
        Self {
            channel_controller,
            created_subchannels: vec![],
            picker_update: None,
        }
    }
}

impl ChannelController for WrappedController<'_> {
    //call into the real channel controller
    fn new_subchannel(&mut self, address: &Address) -> Arc<dyn Subchannel> {
        let subchannel = self.channel_controller.new_subchannel(address);
        self.created_subchannels.push(subchannel.clone());
        subchannel
    }

    fn update_picker(&mut self, update: LbState) {
        self.picker_update = Some(update);
    }

    fn request_resolution(&mut self) {
        self.channel_controller.request_resolution();
    }
}

// ChildPolicy represents a child policy.
struct ChildPolicy {
    policy_builder: Arc<dyn LbPolicyBuilder>,
    policy: Box<dyn LbPolicy>,
    policy_state: ConnectivityState,
    policy_picker_update: Option<LbState>,
}

impl ChildPolicy {
    fn new(
        policy_builder: Arc<dyn LbPolicyBuilder>,
        policy: Box<dyn LbPolicy>,
        policy_state: ConnectivityState,
    ) -> Self {
        ChildPolicy {
            policy_builder,
            policy,
            policy_state,
            policy_picker_update: None,
        }
    }
}

// This ChildPolicyManager keeps track of the current and pending children. It
// keeps track of the latest policy and retrieves it's child policy based on an
// enum.
struct ChildPolicyManager {
    current_child: Option<ChildPolicy>,
    pending_child: Option<ChildPolicy>,
}

impl ChildPolicyManager {
    fn new() -> Self {
        ChildPolicyManager {
            current_child: None,
            pending_child: None,
        }
    }

    fn latest_policy(&mut self) -> String {
        if let Some(pending_child) = &self.pending_child {
            pending_child.policy_builder.name().to_string()
        } else if let Some(current_child) = &self.current_child {
            current_child.policy_builder.name().to_string()
        } else {
            "".to_string()
        }
    }

    fn no_policy(&self) -> bool {
        if self.pending_child.is_none() && self.current_child.is_none() {
            return true;
        }
        false
    }

    fn get_child_policy(&mut self, kind: &ChildKind) -> Option<&mut ChildPolicy> {
        match kind {
            ChildKind::Current => self.current_child.as_mut(),
            ChildKind::Pending => self.pending_child.as_mut(),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::client::channel::WorkQueueItem;
    use crate::client::load_balancing::graceful_switch::{self, GracefulSwitchPolicy};
    use crate::client::load_balancing::test_utils::{
        self, reg_stub_policy, StubPolicyBuilder, StubPolicyData, StubPolicyFuncs,
        TestChannelController, TestEvent, TestSubchannel, TestWorkScheduler,
    };
    use crate::client::load_balancing::{pick_first, LbState, Pick};
    use crate::client::load_balancing::{
        ChannelController, LbPolicy, LbPolicyBuilder, LbPolicyOptions, ParsedJsonLbConfig,
        PickResult, Picker, Subchannel, SubchannelState, GLOBAL_LB_REGISTRY,
    };
    use crate::client::name_resolution::{Address, Endpoint, ResolverUpdate};
    use crate::client::service_config::ServiceConfig;
    use crate::client::ConnectivityState;
    use crate::rt::{default_runtime, Runtime};
    use crate::service::Request;
    use std::collections::HashMap;
    use std::thread::current;
    use std::{panic, sync::Arc};
    use tokio::sync::mpsc;
    use tonic::metadata::MetadataMap;

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
                on_complete: None,
                metadata: MetadataMap::new(),
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
                        Ok(())
                    } else {
                        data.test_data = None;
                        Ok(())
                    }
                },
            )),
            // Closure for subchannel_update. Verify that the subchannel that
            // being updated now is the same one that this child policy created
            // in resolver_update. It then sends a picker of the same state that
            // was passed to it.
            subchannel_update: Some(Arc::new(
                move |data: &mut StubPolicyData, updated_subchannel, state, channel_controller| {
                    // Retrieve the specific TestState from the generic test_data field.
                    // This downcasts the `Any` trait object
                    if let Some(test_data) = data.test_data.as_mut() {
                        if let Some(test_state) = test_data.downcast_mut::<TestState>() {
                            let scl = &mut test_state.subchannel_list;
                            assert!(
                                scl.contains(&updated_subchannel),
                                "subchannel_update received an update for a subchannel it does not own."
                            );
                            channel_controller.update_picker(LbState {
                                connectivity_state: state.connectivity_state,
                                picker: Arc::new(TestPicker { name }),
                            });
                        }
                    }
                },
            )),
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

        let tcc = Box::new(TestChannelController { tx_events });

        let graceful_switch = GracefulSwitchPolicy::new(work_scheduler, default_runtime());
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

    // Verifies that the expected number of subchannels is created. Returns the
    // subchannels created.
    async fn verify_subchannel_creation_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
    ) -> Vec<Arc<dyn Subchannel>> {
        let mut subchannels = Vec::new();
        match rx_events.recv().await.unwrap() {
            TestEvent::NewSubchannel(sc) => {
                subchannels.push(sc);
            }
            other => panic!("unexpected event {:?}", other),
        };
        subchannels
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
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                let req = test_utils::new_request();
                println!("{:?}", update.connectivity_state);

                match update.picker.pick(&req) {
                    PickResult::Pick(pick) => {
                        let received_address = &pick.subchannel.address().address.to_string();
                        // It's good practice to create the expected value once.
                        let expected_address = name.to_string();

                        // Check for inequality and panic with a detailed message if they don't match.
                        if received_address != &expected_address {
                            panic!(
                                "Picker address mismatch. Expected: '{}', but got: '{}'",
                                expected_address, received_address
                            );
                        }
                    }
                    other => panic!("unexpected pick result"),
                }
            }
            other => panic!("unexpected event {:?}", other),
        }
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
        let service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_successful_first_update-one": serde_json::json!({}) },
                { "stub-gracefulswitch_successful_first_update-two": serde_json::json!({}) }
            ]
        });

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

        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        let mut subchannels = subchannels.into_iter();
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannels.next().unwrap(),
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

        let service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_switching_to_resolver_update-one": serde_json::json!({}) }
            ]
        });
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
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        let mut subchannels = subchannels.into_iter();
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannels.next().unwrap(),
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
        let new_service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_switching_to_resolver_update-two": serde_json::json!({}) }
            ]
        });
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();
        graceful_switch
            .resolver_update(update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();

        // Simulate subchannel creation and ready for pending
        let subchannels_two = verify_subchannel_creation_from_policy(&mut rx_events).await;
        let mut subchannels_two = subchannels_two.into_iter();
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannels_two.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );

        // Assert picker is TestPickerTwo by checking subchannel address
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_switching_to_resolver_update-two",
        )
        .await;
    }

    // Tests that the gracefulswitch policy should do nothing when a receives a
    // new config of the same policy that it received before.
    #[tokio::test]
    async fn gracefulswitch_two_policies_same_type() {
        let (mut rx_events, mut graceful_switch, mut tcc) = setup();
        reg_stub_policy(
            "stub-gracefulswitch_two_policies_same_type-one",
            create_funcs_for_gracefulswitch_tests("stub-gracefulswitch_two_policies_same_type-one"),
        );
        let service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_two_policies_same_type-one": serde_json::json!({}) }
            ]
        });
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
        let subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        let mut subchannels = subchannels.into_iter();
        move_subchannel_to_state(
            &mut *graceful_switch,
            subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        let service_config2 = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_two_policies_same_type-one": serde_json::json!({}) }
            ]
        });
        let parsed_config2 = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config2,
        })
        .unwrap();
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config2), &mut *tcc)
            .unwrap();
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

        let service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_current_not_ready_pending_update-one": serde_json::json!({}) }
            ]
        });

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
        let new_service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_current_not_ready_pending_update-two": serde_json::json!({ "shuffleAddressList": false }) },
            ]
        });
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

        let second_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        let mut second_subchannels = second_subchannels.into_iter();
        move_subchannel_to_state(
            &mut *graceful_switch,
            second_subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_not_ready_pending_update-two",
        )
        .await;
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
        let service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_current_leaving_ready-one": serde_json::json!({}) }
            ]
        });
        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();

        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        // let pickfirst_endpoint = create_endpoint_with_one_address("0.0.0.0.0".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        let current_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannels[0].clone(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-one",
        )
        .await;
        let new_service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_current_leaving_ready-two": serde_json::json!({}) },

            ]
        });
        let new_update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();
        graceful_switch
            .resolver_update(new_update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();

        let pending_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;

        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannels[0].clone(),
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-one",
        )
        .await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            pending_subchannels[0].clone(),
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
        let service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_current_leaving_ready-one": serde_json::json!({}) }
            ]
        });
        let parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: service_config,
        })
        .unwrap();
        let endpoint = create_endpoint_with_one_address("127.0.0.1:1234".to_string());
        // let pickfirst_endpoint = create_endpoint_with_one_address("0.0.0.0.0".to_string());
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };

        // Switch to first one (current)
        graceful_switch
            .resolver_update(update.clone(), Some(&parsed_config), &mut *tcc)
            .unwrap();

        let current_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannels[0].clone(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_current_leaving_ready-one",
        )
        .await;
        let new_service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_current_leaving_ready-two": serde_json::json!({}) },
            ]
        });
        let new_update = ResolverUpdate {
            endpoints: Ok(vec![endpoint.clone()]),
            ..Default::default()
        };
        let new_parsed_config = GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig {
            value: new_service_config,
        })
        .unwrap();

        graceful_switch
            .resolver_update(new_update.clone(), Some(&new_parsed_config), &mut *tcc)
            .unwrap();

        let pending_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;

        move_subchannel_to_state(
            &mut *graceful_switch,
            pending_subchannels[0].clone(),
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
            pending_subchannels[0].clone(),
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
    #[should_panic(
        expected = "Subchannel not found in graceful switch: Subchannel: :127.0.0.1:1234"
    )]
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
        let service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one": serde_json::json!({}) }
            ]
        });
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

        let current_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannels[0].clone(),
            tcc.as_mut(),
            ConnectivityState::Ready,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-one",
        )
        .await;
        let new_service_config = serde_json::json!({
            "children_policies": [
                { "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two": serde_json::json!({ "shuffleAddressList": false }) },
            ]
        });
        let second_endpoint = create_endpoint_with_one_address("0.0.0.0.0".to_string());
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
        let pending_subchannels = verify_subchannel_creation_from_policy(&mut rx_events).await;
        let mut pending_subchannels = pending_subchannels.into_iter();
        println!("moving subchannel to idle");
        move_subchannel_to_state(
            &mut *graceful_switch,
            pending_subchannels.next().unwrap(),
            tcc.as_mut(),
            ConnectivityState::Idle,
        );
        verify_correct_picker_from_policy(
            &mut rx_events,
            "stub-gracefulswitch_subchannels_removed_after_current_child_swapped-two",
        )
        .await;
        move_subchannel_to_state(
            &mut *graceful_switch,
            current_subchannels[0].clone(),
            tcc.as_mut(),
            ConnectivityState::Connecting,
        );
    }
}
