use std::{
    collections::{HashMap, HashSet},
    error::Error,
    hash::Hash,
    ops::Sub,
    sync::{Arc, LazyLock, Mutex, Once},
    time::Duration,
};

use crate::{
    client::{
        load_balancing::{
            ChannelController, ExternalSubchannel, Failing, LbPolicy, LbPolicyBuilder,
            LbPolicyOptions, LbState, ParsedJsonLbConfig, Pick, PickResult, Picker, QueuingPicker,
            Subchannel, SubchannelState, WorkScheduler, GLOBAL_LB_REGISTRY,
        },
        name_resolution::{Address, Endpoint, ResolverUpdate},
        service_config::LbConfig,
        subchannel, ConnectivityState,
    },
    service::{Request, Response, Service},
};

use once_cell::sync::Lazy;
use rand::{self, rng, rngs::StdRng, seq::SliceRandom, Rng, RngCore, SeedableRng};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::sleep;
use tonic::{async_trait, metadata::MetadataMap};

// A function type that takes a mutable slice of endpoints and shuffles them.
type EndpointShuffler = dyn Fn(&mut [Endpoint]) + Send + Sync + 'static;

// A global shuffler function that can be overridden for testing.
pub static SHUFFLE_ENDPOINTS_FN: LazyLock<Mutex<Box<EndpointShuffler>>> =
    std::sync::LazyLock::new(|| {
        let shuffle_endpoints = thread_rng_shuffler();
        Mutex::new(shuffle_endpoints)
    });
pub(crate) fn thread_rng_shuffler() -> Box<EndpointShuffler> {
    Box::new(|endpoints: &mut [Endpoint]| {
        let mut rng = rng();
        endpoints.shuffle(&mut rng);
    })
}

pub static POLICY_NAME: &str = "pick_first";

struct Builder {}

impl LbPolicyBuilder for Builder {
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy> {
        Box::new(PickFirstPolicy {
            work_scheduler: options.work_scheduler,
            subchannel_list: None,
            selected_subchannel: None,
            addresses: vec![],
            last_resolver_error: None,
            last_connection_error: None,
            connectivity_state: ConnectivityState::Connecting,
            sent_connecting_state: false,
            num_transient_failures: 0,
        })
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }

    fn parse_config(
        &self,
        config: &ParsedJsonLbConfig,
    ) -> Result<Option<LbConfig>, Box<dyn Error + Send + Sync>> {
        let cfg: PickFirstConfig = match config.convert_to() {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("failed to parse JSON config: {}", e).into());
            }
        };
        Ok(Some(LbConfig::new(cfg)))
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(super) struct PickFirstConfig {
    shuffle_address_list: Option<bool>,
}

pub fn reg() {
    static REGISTER_ONCE: Once = Once::new();
    REGISTER_ONCE.call_once(|| {
        GLOBAL_LB_REGISTRY.add_builder(Builder {});
    });
}

struct PickFirstPolicy {
    work_scheduler: Arc<dyn WorkScheduler>, // Helps to schedule work.
    subchannel_list: Option<SubchannelList>, // List of subchannels, that we are currently connecting to.
    selected_subchannel: Option<Arc<dyn Subchannel>>, // The currently connected subchannel.
    addresses: Vec<Address>,                 // Most recent addresses from the name resolver.
    last_resolver_error: Option<String>,     // Most recent error from the name resolver.
    last_connection_error: Option<Arc<dyn Error + Send + Sync>>, // Most recent error from any subchannel.
    connectivity_state: ConnectivityState, // Overall connectivity state of the channel.
    sent_connecting_state: bool, // Whether we have sent a CONNECTING state to the channel.
    num_transient_failures: usize, // Number of transient failures after the end of the first pass.
}

impl LbPolicy for PickFirstPolicy {
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match update.endpoints {
            Ok(mut endpoints) => {
                println!(
                    "received update from resolver with endpoints: {:?}",
                    endpoints
                );

                // Shuffle endpoints if requested in the LB config.
                if let Some(err) = self.shuffle_endpoints(config, &mut endpoints) {
                    println!("failed to shuffle endpoints: {}", err);
                    return Err(err);
                }

                // Perform other address list handling as specified in A61.
                let new_addresses: Vec<Address> = self.address_list_from_endpoints(&endpoints);

                // Treat empty resolver updates identically to resolver errors
                // that occur before any valid update has been received.
                if new_addresses.is_empty() {
                    self.handle_empty_endpoints(channel_controller);
                    return Err("received empty address list from the name resolver".into());
                }

                // Start using the new address list unless in IDLE, in which
                // case, we rely on exit_idle() for the same.
                if self.connectivity_state != ConnectivityState::Idle {
                    self.subchannel_list =
                        Some(SubchannelList::new(&new_addresses, channel_controller));
                }
                self.addresses = new_addresses;
            }
            Err(error) => {
                println!("received error from resolver: {}", error);
                self.last_resolver_error = Some(error);

                // Enter or stay in TF, if there is no good previous update from
                // the resolver, or if already in TF. Regardless, send a new
                // failing picker with the updated error information.
                if self.addresses.is_empty()
                    || self.connectivity_state == ConnectivityState::TransientFailure
                {
                    self.move_to_transient_failure(channel_controller);
                }

                // Continue using the previous good update, if one exists.
            }
        }
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        println!("received update for {}: {}", subchannel, state);

        // Handle the update for this subchannel, provided it's included in the
        // subchannel list (if the list exists).
        if let Some(subchannel_list) = &self.subchannel_list {
            if subchannel_list.contains(&subchannel) {
                if state.connectivity_state == ConnectivityState::Ready {
                    self.move_to_ready(subchannel, channel_controller);
                } else {
                    self.update_tracked_subchannel(subchannel, state, channel_controller);
                }
                return;
            }
        }

        // Handle updates for the currently selected subchannel. Any state
        // change for the currently connected subchannel means that we are no
        // longer connected.
        if let Some(selected_sc) = &self.selected_subchannel {
            if *selected_sc == subchannel.clone() {
                self.move_to_idle(channel_controller);
                return;
            }
        }

        debug_assert!(
            false,
            "received update for unknown subchannel: {}",
            subchannel
        );
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        // Build a new subchannel list with the most recent addresses received
        // from the name resolver. This will start connecting from the first
        // address in the list.
        self.subchannel_list = Some(SubchannelList::new(&self.addresses, channel_controller));
    }
}

impl PickFirstPolicy {
    fn shuffle_endpoints(
        &self,
        config: Option<&LbConfig>,
        endpoints: &mut [Endpoint],
    ) -> Option<Box<dyn Error + Send + Sync>> {
        config?;

        let cfg: Arc<PickFirstConfig> = match config.unwrap().convert_to() {
            Ok(cfg) => cfg,
            Err(e) => return Some(e),
        };
        println!("received update from resolver with config: {:?}", &cfg);

        let mut shuffle_addresses = false;
        if let Some(v) = cfg.shuffle_address_list {
            shuffle_addresses = v;
        }

        // Perform the optional shuffling described in A62. The shuffling will
        // change the order of the endpoints but will not touch the order of the
        // addresses within each endpoint - A61.
        if shuffle_addresses {
            SHUFFLE_ENDPOINTS_FN.lock().unwrap()(endpoints);
        };
        None
    }

    fn address_list_from_endpoints(&self, endpoints: &[Endpoint]) -> Vec<Address> {
        // Flatten the endpoints list by concatenating the ordered list of
        // addresses for each of the endpoints.
        let mut addresses: Vec<Address> = endpoints
            .iter()
            .flat_map(|ep| ep.addresses.clone())
            .collect();

        // Remove duplicates.
        let mut uniques = HashSet::new();
        addresses.retain(|e| uniques.insert(e.clone()));

        // TODO(easwars): Implement address family interleaving as part of
        // the dualstack implementation.

        addresses
    }

    // Handles the case when the resolver returns an empty address list. Resets
    // internal state and moves to TRANSIENT_FAILURE.
    fn handle_empty_endpoints(&mut self, channel_controller: &mut dyn ChannelController) {
        self.subchannel_list = None;
        self.selected_subchannel = None;
        self.addresses = vec![];
        let res_err = String::from("received empty address list from the name resolver");
        self.last_resolver_error = Some(res_err);
        self.move_to_transient_failure(channel_controller);
        channel_controller.request_resolution();
    }

    // Handles updates for subchannels currently in the subchannel list.
    fn update_tracked_subchannel(
        &mut self,
        sc: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        let subchannel_list = self.subchannel_list.as_mut().unwrap();

        // Update subchannel data. Return early if not all subchannels have seen
        // their first state update.
        let old_state = subchannel_list.update_subchannel_data(&sc, state);
        if !subchannel_list.all_subchannels_seen_initial_state() {
            return;
        }

        // We're here only if all subchannels have seen their first update.

        // Handle the last subchannel to report its initial state.
        if old_state.is_none() {
            if self.selected_subchannel.is_some() {
                // Close the selected subchannel and go IDLE because it is no
                // longer part of the most recent update from the resolver. We
                // handle subchannel state transitions to READY much earlier in
                // subchannel_update().
                self.move_to_idle(channel_controller);
            } else {
                // Start connecting from the first subchannel.
                if !subchannel_list.connect_to_next_subchannel(channel_controller) {
                    debug_assert!(false, "failed to initiate connection to first subchannel");
                }
            }
            return;
        }

        // Otherwise, handle the most recent subchannel state transition.
        match state.connectivity_state {
            ConnectivityState::Idle => {
                // Immediately connect to subchannels transitioning to IDLE,
                // once the first pass is complete.
                if subchannel_list.is_first_pass_complete() {
                    sc.connect();
                }
            }
            ConnectivityState::Connecting => {
                // If we are already in CONNECTING, ignore this update.
                if self.connectivity_state == ConnectivityState::Connecting
                    && self.sent_connecting_state
                {
                    return;
                }
                if self.connectivity_state != ConnectivityState::TransientFailure {
                    self.move_to_connecting(channel_controller);
                }
            }
            ConnectivityState::TransientFailure => {
                self.last_connection_error = state.last_connection_error.clone();

                if !subchannel_list.is_first_pass_complete() {
                    // Connect to the next subchannel in the list.
                    if !subchannel_list.connect_to_next_subchannel(channel_controller) {
                        // TODO(easwars): Call go_transient_failure() instead.
                        // Currently, doing this fails the borrow checker.

                        // Move to TRANSIENT_FAILURE and attempt to connect to
                        // all subchannels once we get to the end of the list.
                        self.connectivity_state = ConnectivityState::TransientFailure;
                        let err = format!(
                            "last seen resolver error: {:?}, last seen connection error: {:?}",
                            self.last_resolver_error, self.last_connection_error,
                        );
                        channel_controller.update_picker(LbState {
                            connectivity_state: ConnectivityState::TransientFailure,
                            picker: Arc::new(Failing { error: err }),
                        });
                        channel_controller.request_resolution();
                        println!("first pass complete, connecting to all subchannels");
                        subchannel_list.connect_to_all_subchannels(channel_controller);
                    }
                } else {
                    self.num_transient_failures += 1;
                    if self.num_transient_failures == subchannel_list.len() {
                        // Request re-resolution and update the error picker.
                        self.move_to_transient_failure(channel_controller);
                        self.num_transient_failures = 0;
                    }
                }
            }
            _ => {
                debug_assert!(
                    false,
                    "unexpected state transition for subchannel {}: {:?} -> {:?}",
                    sc,
                    old_state.unwrap().connectivity_state,
                    state.connectivity_state
                );
            }
        }
    }

    fn move_to_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.connectivity_state = ConnectivityState::Idle;
        self.subchannel_list = None;
        self.selected_subchannel = None;
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::Idle,
            picker: Arc::new(IdlePicker {
                work_scheduler: self.work_scheduler.clone(),
            }),
        });
        channel_controller.request_resolution();
        self.sent_connecting_state = false;
    }

    fn move_to_connecting(&mut self, channel_controller: &mut dyn ChannelController) {
        self.connectivity_state = ConnectivityState::Connecting;
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::Connecting,
            picker: Arc::new(QueuingPicker {}),
        });
        self.sent_connecting_state = true;
    }

    fn move_to_ready(
        &mut self,
        sc: Arc<dyn Subchannel>,
        channel_controller: &mut dyn ChannelController,
    ) {
        self.connectivity_state = ConnectivityState::Ready;
        self.selected_subchannel = Some(sc.clone());
        self.subchannel_list = None;
        self.last_connection_error = None;
        self.last_resolver_error = None;
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::Ready,
            picker: Arc::new(OneSubchannelPicker { sc: sc.clone() }),
        });
        self.sent_connecting_state = false;
    }

    fn move_to_transient_failure(&mut self, channel_controller: &mut dyn ChannelController) {
        self.connectivity_state = ConnectivityState::TransientFailure;
        let err = format!(
            "last seen resolver error: {:?}, last seen connection error: {:?}",
            self.last_resolver_error, self.last_connection_error,
        );
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::TransientFailure,
            picker: Arc::new(Failing { error: err }),
        });
        channel_controller.request_resolution();
        self.sent_connecting_state = false;
    }
}

// A picker that always returns the same subchannel.
struct OneSubchannelPicker {
    sc: Arc<dyn Subchannel>,
}

impl Picker for OneSubchannelPicker {
    fn pick(&self, request: &Request) -> PickResult {
        PickResult::Pick(Pick {
            subchannel: self.sc.clone(),
            on_complete: None,
            metadata: MetadataMap::new(),
        })
    }
}

// A picker that always queues picks and schedules work. This triggers the LB
// policy to start connecting from the first address.
pub struct IdlePicker {
    work_scheduler: Arc<dyn WorkScheduler>,
}

impl Picker for IdlePicker {
    fn pick(&self, request: &Request) -> PickResult {
        self.work_scheduler.schedule_work();
        PickResult::Queue
    }
}

// Data tracked for each subchannel in the subchannel list.
#[derive(Clone)]
struct SubchannelData {
    state: Option<SubchannelState>,
    seen_transient_failure: bool,
}

impl SubchannelData {
    fn new() -> SubchannelData {
        SubchannelData {
            state: None,
            seen_transient_failure: false,
        }
    }
}

// A list of subchannels created from the most recent address list from the
// resolver.
//
// The list tracks the state of each subchannel, and helps to manage connection
// attempts to the subchannels in the list.
struct SubchannelList {
    subchannels: HashMap<Arc<dyn Subchannel>, SubchannelData>,
    ordered_subchannels: Vec<Arc<dyn Subchannel>>,
    current_idx: usize,
    num_initial_notifications_seen: usize,
}

impl SubchannelList {
    fn new(addresses: &Vec<Address>, channel_controller: &mut dyn ChannelController) -> Self {
        let mut scl = SubchannelList {
            subchannels: HashMap::new(),
            ordered_subchannels: Vec::new(),
            current_idx: 0,
            num_initial_notifications_seen: 0,
        };
        for address in addresses {
            let sc = channel_controller.new_subchannel(address);
            scl.ordered_subchannels.push(sc.clone());
            scl.subchannels.insert(sc, SubchannelData::new());
        }

        println!("created new subchannel list with {} subchannels", scl.len());
        scl
    }

    fn len(&self) -> usize {
        self.ordered_subchannels.len()
    }

    fn subchannel_data(&self, sc: &Arc<dyn Subchannel>) -> Option<SubchannelData> {
        self.subchannels.get(sc).cloned()
    }

    fn contains(&self, sc: &Arc<dyn Subchannel>) -> bool {
        self.subchannels.contains_key(sc)
    }

    // Updates internal state of the subchannel with the new state. Callers must
    // ensure that this method is called only for subchannels in the list.
    //
    // Returns old state corresponding to the subchannel, if one exists.
    fn update_subchannel_data(
        &mut self,
        sc: &Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) -> Option<SubchannelState> {
        let sc_data = self.subchannels.get_mut(sc).unwrap();

        // Increment the counter when seeing the first update.
        if sc_data.state.is_none() {
            self.num_initial_notifications_seen += 1;
        }

        let old_state = sc_data.state.clone();
        sc_data.state = Some(state.clone());
        match state.connectivity_state {
            ConnectivityState::Ready => sc_data.seen_transient_failure = false,
            ConnectivityState::TransientFailure => sc_data.seen_transient_failure = true,
            _ => {}
        }

        old_state
    }

    fn all_subchannels_seen_initial_state(&self) -> bool {
        self.num_initial_notifications_seen == self.ordered_subchannels.len()
    }

    // Initiates a connection attempt to the next subchannel in the list that is
    // IDLE. Returns false if there are no more subchannels in the list.
    fn connect_to_next_subchannel(
        &mut self,
        channel_controller: &mut dyn ChannelController,
    ) -> bool {
        // Special case for the first connection attempt, as current_idx is set
        // to 0 when the subchannel list is created.
        if self.current_idx != 0 {
            self.current_idx += 1;
        }

        for idx in self.current_idx..self.ordered_subchannels.len() {
            // Grab the next subchannel and its data.
            let sc = &self.ordered_subchannels[idx];
            let sc_data = self.subchannels.get(sc).unwrap();

            match &sc_data.state {
                Some(state) => {
                    if state.connectivity_state == ConnectivityState::Connecting
                        || state.connectivity_state == ConnectivityState::TransientFailure
                    {
                        self.current_idx += 1;
                        continue;
                    } else if state.connectivity_state == ConnectivityState::Idle {
                        sc.connect();
                        return true;
                    }
                }
                None => {
                    debug_assert!(
                        false,
                        "No state available when asked to connect to subchannel: {}",
                        sc,
                    );
                }
            }
        }
        false
    }

    fn is_first_pass_complete(&self) -> bool {
        if self.current_idx < self.ordered_subchannels.len() {
            return false;
        }
        for data in self.subchannels.values() {
            if !data.seen_transient_failure {
                return false;
            }
        }
        true
    }

    fn connect_to_all_subchannels(&mut self, channel_controller: &mut dyn ChannelController) {
        for (sc, data) in &mut self.subchannels {
            if data.state.as_ref().unwrap().connectivity_state == ConnectivityState::Idle {
                sc.connect();
            }
        }
    }
}

#[cfg(test)]
mod test {
    use crate::client::{
        load_balancing::{
            pick_first::{
                self, thread_rng_shuffler, EndpointShuffler, PickFirstConfig, SHUFFLE_ENDPOINTS_FN,
            },
            test_utils::{self, TestChannelController, TestEvent, TestWorkScheduler},
            ChannelController, ExternalSubchannel, Failing, LbConfig, LbPolicy, LbPolicyBuilder,
            LbPolicyOptions, LbState, ParsedJsonLbConfig, PickResult, Picker, QueuingPicker,
            Subchannel, SubchannelState, WorkScheduler, GLOBAL_LB_REGISTRY,
        },
        name_resolution::{Address, Endpoint, ResolverUpdate},
        transport::{Transport, GLOBAL_TRANSPORT_REGISTRY},
        ConnectivityState,
    };
    use crate::service::{Message, Request, Response, Service};
    use core::panic;
    use rand::{rngs::StdRng, Rng, RngCore, SeedableRng};
    use serde_json::json;
    use std::{
        ops::Add,
        sync::{Arc, Mutex},
    };
    use tokio::{
        sync::{mpsc, Notify},
        task::AbortHandle,
    };
    use tonic::async_trait;

    #[test]
    fn pickfirst_builder_name() -> Result<(), String> {
        pick_first::reg();

        let builder: Arc<dyn LbPolicyBuilder> = match GLOBAL_LB_REGISTRY.get_policy("pick_first") {
            Some(b) => b,
            None => {
                return Err(String::from("pick_first LB policy not registered"));
            }
        };
        assert_eq!(builder.name(), "pick_first");
        Ok(())
    }

    #[test]
    fn pickfirst_builder_parse_config_failure() -> Result<(), String> {
        pick_first::reg();

        let builder: Arc<dyn LbPolicyBuilder> = match GLOBAL_LB_REGISTRY.get_policy("pick_first") {
            Some(b) => b,
            None => {
                return Err(String::from("pick_first LB policy not registered"));
            }
        };

        // Success cases.
        struct TestCase {
            config: ParsedJsonLbConfig,
            want_shuffle_addresses: Option<bool>,
        }
        let test_cases = vec![
            TestCase {
                config: ParsedJsonLbConfig::from_value(json!({})),
                want_shuffle_addresses: None,
            },
            TestCase {
                config: ParsedJsonLbConfig::from_value(json!({"shuffleAddressList": false})),
                want_shuffle_addresses: Some(false),
            },
            TestCase {
                config: ParsedJsonLbConfig::from_value(json!({"shuffleAddressList": true})),
                want_shuffle_addresses: Some(true),
            },
            TestCase {
                config: ParsedJsonLbConfig::from_value(
                    json!({"shuffleAddressList": true, "unknownField": "foo"}),
                ),
                want_shuffle_addresses: Some(true),
            },
        ];
        for tc in test_cases {
            let config = match builder.parse_config(&tc.config) {
                Ok(c) => c,
                Err(e) => {
                    let err = format!(
                        "parse_config({:?}) failed when expected to succeed: {:?}",
                        tc.config, e
                    )
                    .clone();
                    panic!("{}", err);
                }
            };
            let config: LbConfig = match config {
                Some(c) => c,
                None => {
                    let err = format!(
                        "parse_config({:?}) returned None when expected to succeed",
                        tc.config
                    )
                    .clone();
                    panic!("{}", err);
                }
            };
            let got_config: Arc<PickFirstConfig> = config.convert_to().unwrap();
            assert!(got_config.shuffle_address_list == tc.want_shuffle_addresses);
        }
        Ok(())
    }

    // Sets up the test environment.
    //
    // Performs the following:
    // 1. Creates a work scheduler.
    // 2. Creates a fake channel that acts as a channel controller.
    // 3. Creates a pick_first LB policy.
    //
    // Returns the following:
    // 1. A receiver for events initiated by the LB policy (like creating a
    //    new subchannel, sending a new picker etc).
    // 2. The LB policy to send resolver and subchannel updates from the test.
    // 3. The controller to pass to the LB policy as part of the updates.
    fn setup() -> (
        mpsc::UnboundedReceiver<TestEvent>,
        Box<dyn LbPolicy>,
        Box<dyn ChannelController>,
    ) {
        pick_first::reg();
        let (tx_events, rx_events) = mpsc::unbounded_channel::<TestEvent>();
        let work_scheduler = Arc::new(TestWorkScheduler {
            tx_events: tx_events.clone(),
        });
        let tcc = Box::new(TestChannelController {
            tx_events: tx_events.clone(),
        });
        let builder: Arc<dyn LbPolicyBuilder> =
            GLOBAL_LB_REGISTRY.get_policy("pick_first").unwrap();
        let lb_policy = builder.build(LbPolicyOptions { work_scheduler });

        (rx_events, lb_policy, tcc)
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

    // Creates a new endpoint with the specified number of addresses.
    fn create_endpoint_with_n_addresses(n: usize) -> Endpoint {
        let mut addresses = Vec::new();
        for i in 0..n {
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

    // Sends a resolver update to the LB policy with the specified endpoint.
    fn send_resolver_update_to_policy(
        lb_policy: &mut dyn LbPolicy,
        endpoints: Vec<Endpoint>,
        tcc: &mut dyn ChannelController,
    ) {
        let update = ResolverUpdate {
            endpoints: Ok(endpoints.clone()),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_ok());
    }

    // Sends a resolver update with LB config enabling address shuffling to the LB
    // policy with the specified endpoint.
    fn send_resolver_update_with_lb_config_to_policy(
        lb_policy: &mut dyn LbPolicy,
        endpoints: Vec<Endpoint>,
        tcc: &mut dyn ChannelController,
    ) {
        let update = ResolverUpdate {
            endpoints: Ok(endpoints.clone()),
            ..Default::default()
        };

        let json_config = ParsedJsonLbConfig::from_value(json!({"shuffleAddressList": true}));
        let builder = GLOBAL_LB_REGISTRY.get_policy("pick_first").unwrap();
        let config = builder.parse_config(&json_config).unwrap();

        assert!(lb_policy
            .resolver_update(update, config.as_ref(), tcc)
            .is_ok());
    }

    // Sends a resolver error to the LB policy with the specified error message.
    fn send_resolver_error_to_policy(
        lb_policy: &mut dyn LbPolicy,
        err: String,
        tcc: &mut dyn ChannelController,
    ) {
        let update = ResolverUpdate {
            endpoints: Err(err),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_ok());
    }

    // Verifies that the subchannels are created for the given addresses in the
    // given order. Returns the subchannels created.
    async fn verify_subchannel_creation_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        addresses: Vec<Address>,
    ) -> Vec<Arc<dyn Subchannel>> {
        let mut subchannels = Vec::new();
        for address in addresses {
            match rx_events.recv().await.unwrap() {
                TestEvent::NewSubchannel(addr, sc) => {
                    assert!(addr == address.clone());
                    subchannels.push(sc);
                }
                other => panic!("unexpected event {}", other),
            };
        }
        subchannels
    }

    // Sends initial subchannel updates to the LB policy for the given
    // subchannels, with their state set to IDLE.
    fn send_initial_subchannel_updates_to_policy(
        lb_policy: &mut dyn LbPolicy,
        subchannels: &[Arc<dyn Subchannel>],
        tcc: &mut dyn ChannelController,
    ) {
        for sc in subchannels {
            lb_policy.subchannel_update(sc.clone(), &SubchannelState::default(), tcc);
        }
    }

    fn move_subchannel_to_idle(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                ..Default::default()
            },
            tcc,
        );
    }

    fn move_subchannel_to_connecting(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Connecting,
                ..Default::default()
            },
            tcc,
        );
    }

    fn move_subchannel_to_ready(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                ..Default::default()
            },
            tcc,
        );
    }

    fn move_subchannel_to_transient_failure(
        lb_policy: &mut dyn LbPolicy,
        subchannel: Arc<dyn Subchannel>,
        err: &str,
        tcc: &mut dyn ChannelController,
    ) {
        lb_policy.subchannel_update(
            subchannel.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some(Arc::from(Box::from(err.to_owned()))),
            },
            tcc,
        );
    }

    // Verifies that a connection attempt is made to the given subchannel.
    async fn verify_connection_attempt_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        subchannel: Arc<dyn Subchannel>,
    ) {
        match rx_events.recv().await.unwrap() {
            TestEvent::Connect(addr) => {
                assert!(addr == subchannel.address());
            }
            other => panic!("unexpected event {}", other),
        };
    }

    // Verifies that a call to schedule_work is made by the LB policy.
    async fn verify_schedule_work_from_policy(rx_events: &mut mpsc::UnboundedReceiver<TestEvent>) {
        match rx_events.recv().await.unwrap() {
            TestEvent::ScheduleWork => {}
            other => panic!("unexpected event {}", other),
        };
    }

    // Verifies that the channel moves to IDLE state.
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_idle_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
    ) -> Arc<dyn Picker> {
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                assert!(update.connectivity_state == ConnectivityState::Idle);
                update.picker.clone()
            }
            other => panic!("unexpected event {}", other),
        }
    }

    // Verifies that the channel moves to CONNECTING state with a queuing picker.
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_connecting_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
    ) -> Arc<dyn Picker> {
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                assert!(update.connectivity_state == ConnectivityState::Connecting);
                let req = test_utils::new_request();
                assert!(update.picker.pick(&req) == PickResult::Queue);
                update.picker.clone()
            }
            other => panic!("unexpected event {}", other),
        }
    }

    // Verifies that the channel moves to READY state with a picker that returns the
    // given subchannel.
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_ready_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        subchannel: Arc<dyn Subchannel>,
    ) -> Arc<dyn Picker> {
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                assert!(update.connectivity_state == ConnectivityState::Ready);
                let req = test_utils::new_request();
                match update.picker.pick(&req) {
                    PickResult::Pick(pick) => {
                        assert!(pick.subchannel == subchannel.clone());
                        update.picker.clone()
                    }
                    other => panic!("unexpected pick result {}", other),
                }
            }
            other => panic!("unexpected event {}", other),
        }
    }

    // Verifies that the channel moves to TRANSIENT_FAILURE state with a picker
    // that returns an error with the given message. The error code should be
    // UNAVAILABLE..
    //
    // Returns the picker for tests to make more picks, if required.
    async fn verify_transient_failure_picker_from_policy(
        rx_events: &mut mpsc::UnboundedReceiver<TestEvent>,
        want_error: String,
    ) -> Arc<dyn Picker> {
        let picker = match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                assert!(update.connectivity_state == ConnectivityState::TransientFailure);
                let req = test_utils::new_request();
                match update.picker.pick(&req) {
                    PickResult::Fail(status) => {
                        assert!(status.code() == tonic::Code::Unavailable);
                        assert!(status.message().contains(&want_error));
                        update.picker.clone()
                    }
                    other => panic!("unexpected pick result {}", other),
                }
            }
            other => panic!("unexpected event {}", other),
        };
        match rx_events.recv().await.unwrap() {
            TestEvent::RequestResolution => {}
            _ => panic!("no re-resolution requested after moving to transient_failure"),
        }
        picker
    }

    // Verifies that the channel moves to IDLE state.
    async fn verify_channel_moves_to_idle(rx_events: &mut mpsc::UnboundedReceiver<TestEvent>) {
        match rx_events.recv().await.unwrap() {
            TestEvent::UpdatePicker(update) => {
                assert!(update.connectivity_state == ConnectivityState::Idle);
            }
            other => panic!("unexpected event {}", other),
        };
    }

    // Verifies that the LB policy requests re-resolution.
    async fn verify_resolution_request(rx_events: &mut mpsc::UnboundedReceiver<TestEvent>) {
        match rx_events.recv().await.unwrap() {
            TestEvent::RequestResolution => {}
            other => panic!("unexpected event {}", other),
        };
    }

    const DEFAULT_TEST_SHORT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

    async fn verify_no_activity_from_policy(rx_events: &mut mpsc::UnboundedReceiver<TestEvent>) {
        tokio::select! {
            _ = tokio::time::sleep(DEFAULT_TEST_SHORT_TIMEOUT) => {}
            event = rx_events.recv() => {
                panic!("unexpected event {}", event.unwrap());
            }
        }
    }

    // Tests the scenario where the resolver returns an error before a valid update.
    // The LB policy should move to TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn pickfirst_resolver_error_before_a_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);
        verify_transient_failure_picker_from_policy(&mut rx_events, resolver_error).await;
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy has moved to READY. The LB policy should ignore the error
    // and continue using the previously received update.
    #[tokio::test]
    async fn pickfirst_resolver_error_after_a_valid_update_in_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);

        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);

        let picker = verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);
        verify_no_activity_from_policy(&mut rx_events).await;

        let req = test_utils::new_request();
        match picker.pick(&req) {
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
    async fn pickfirst_resolver_error_after_a_valid_update_in_connecting() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);

        let picker = verify_connecting_picker_from_policy(&mut rx_events).await;

        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);
        verify_no_activity_from_policy(&mut rx_events).await;

        let req = test_utils::new_request();
        match picker.pick(&req) {
            PickResult::Queue => {}
            other => panic!("unexpected pick result {}", other),
        }
    }

    // Tests the scenario where the resolver returns an error after a valid update
    // and the LB policy has moved to TRANSIENT_FAILURE after attemting to connect
    // to all addresses.  The LB policy should send a new picker that returns the
    // error from the resolver.
    #[tokio::test]
    async fn pickfirst_resolver_error_after_a_valid_update_in_tf() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;

        let connection_error = String::from("test connection error");
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[0].clone(),
            &connection_error,
            tcc,
        );
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[1].clone(), tcc);
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[1].clone(),
            &connection_error,
            tcc,
        );
        verify_transient_failure_picker_from_policy(&mut rx_events, connection_error).await;

        let resolver_error = String::from("resolver error");
        send_resolver_error_to_policy(lb_policy, resolver_error.clone(), tcc);
        verify_transient_failure_picker_from_policy(&mut rx_events, resolver_error).await;
    }

    // Tests the scenario where the resolver returns an update with no addresses
    // (before sending any valid update). The LB policy should move to
    // TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn pickfirst_zero_addresses_from_resolver_before_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(0);
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoint]),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_err());
        verify_transient_failure_picker_from_policy(
            &mut rx_events,
            "received empty address list from the name resolver".to_string(),
        )
        .await;
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // (before sending any valid update). The LB policy should move to
    // TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn pickfirst_zero_endpoints_from_resolver_before_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let update = ResolverUpdate {
            endpoints: Ok(vec![]),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_err());
        verify_transient_failure_picker_from_policy(
            &mut rx_events,
            "received empty address list from the name resolver".to_string(),
        )
        .await;
    }

    // Tests the scenario where the resolver returns an update with no endpoints
    // after sending a valid update (and the LB policy has moved to READY). The LB
    // policy should move to TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn pickfirst_zero_endpoints_from_resolver_after_valid_update() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);

        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);

        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        let update = ResolverUpdate {
            endpoints: Ok(vec![]),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_err());
        verify_transient_failure_picker_from_policy(
            &mut rx_events,
            "received empty address list from the name resolver".to_string(),
        )
        .await;
    }

    // Tests the scenario where the resolver returns an update with one address. The
    // LB policy should create a subchannel for that address, connect to it, and
    // once the connection succeeds, move to READY state with a picker that returns
    // that subchannel.
    #[tokio::test]
    async fn pickfirst_with_one_backend() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(1);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);

        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);

        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // address. The LB policy should create subchannels for all address, and attempt
    // to connect to them in order, until a connection succeeds, at which point it
    // should move to READY state with a picker that returns that subchannel.
    #[tokio::test]
    async fn pickfirst_with_multiple_backends_first_backend_is_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);

        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);

        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // address. The LB policy should create subchannels for all address, and attempt
    // to connect to them in order, until a connection succeeds, at which point it
    // should move to READY state with a picker that returns that subchannel.
    #[tokio::test]
    async fn pickfirst_with_multiple_backends_first_backend_is_not_ready() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(3);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        let connection_error = String::from("test connection error");
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[0].clone(),
            &connection_error,
            tcc,
        );

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[1].clone(), tcc);
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[1].clone(),
            &connection_error,
            tcc,
        );

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[2].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[2].clone(), tcc);
        move_subchannel_to_ready(lb_policy, subchannels[2].clone(), tcc);

        verify_ready_picker_from_policy(&mut rx_events, subchannels[2].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // address, some of which are duplicates. The LB policy should dedup the
    // addresses and create subchannels for them, and attempt to connect to them in
    // order, until a connection succeeds, at which point it should move to READY
    // state with a picker that returns that subchannel.
    #[tokio::test]
    async fn pickfirst_with_multiple_backends_duplicate_addresses() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = Endpoint {
            addresses: vec![
                Address {
                    address: format!("{}.{}.{}.{}:{}", 0, 0, 0, 0, 0).into(),
                    ..Default::default()
                },
                Address {
                    address: format!("{}.{}.{}.{}:{}", 0, 0, 0, 0, 0).into(),
                    ..Default::default()
                },
                Address {
                    address: format!("{}.{}.{}.{}:{}", 1, 1, 1, 1, 1).into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let endpoint_with_duplicates_removed = Endpoint {
            addresses: vec![
                Address {
                    address: format!("{}.{}.{}.{}:{}", 0, 0, 0, 0, 0).into(),
                    ..Default::default()
                },
                Address {
                    address: format!("{}.{}.{}.{}:{}", 1, 1, 1, 1, 1).into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels = verify_subchannel_creation_from_policy(
            &mut rx_events,
            endpoint_with_duplicates_removed.addresses.clone(),
        )
        .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        let connection_error = String::from("test connection error");
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[0].clone(),
            &connection_error,
            tcc,
        );

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[1].clone(), tcc);
        move_subchannel_to_ready(lb_policy, subchannels[1].clone(), tcc);

        verify_ready_picker_from_policy(&mut rx_events, subchannels[1].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and connections to all of them fail. The LB policy should move to
    // TRANSIENT_FAILURE state with a failing picker. It should then attempt to connect
    // to the addresses again, and when they fail again, it should send a new
    // picker that returns the most recent error message.
    #[tokio::test]
    async fn pickfirst_sticky_transient_failure() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoint = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoint.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoint.addresses.clone())
                .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        let first_error = String::from("test connection error 1");
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_transient_failure(lb_policy, subchannels[0].clone(), &first_error, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[1].clone(), tcc);
        move_subchannel_to_transient_failure(lb_policy, subchannels[1].clone(), &first_error, tcc);
        verify_transient_failure_picker_from_policy(&mut rx_events, first_error).await;

        // The subchannels need to complete their backoff before moving to IDLE, at
        // which point the LB policy should attempt to connect to them again.
        move_subchannel_to_idle(lb_policy, subchannels[0].clone(), tcc);
        move_subchannel_to_idle(lb_policy, subchannels[1].clone(), tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;

        let second_error = String::from("test connection error 2");
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        move_subchannel_to_transient_failure(lb_policy, subchannels[0].clone(), &second_error, tcc);
        move_subchannel_to_connecting(lb_policy, subchannels[1].clone(), tcc);
        move_subchannel_to_transient_failure(lb_policy, subchannels[1].clone(), &second_error, tcc);
        verify_transient_failure_picker_from_policy(&mut rx_events, second_error).await;

        // The subchannels need to complete their backoff before moving to IDLE, at
        // which point the LB policy should attempt to connect to them again.
        move_subchannel_to_idle(lb_policy, subchannels[0].clone(), tcc);
        move_subchannel_to_idle(lb_policy, subchannels[1].clone(), tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Overrides the default shuffler function with a custom one that reverses the
    // order of the endpoints.
    fn test_reverse_shuffler() -> Box<EndpointShuffler> {
        Box::new(|endpoints: &mut [Endpoint]| {
            endpoints.reverse();
        })
    }

    // Resets the shuffler function to the default one after the test completes
    struct ShufflerResetGuard {}
    impl Drop for ShufflerResetGuard {
        fn drop(&mut self) {
            *SHUFFLE_ENDPOINTS_FN.lock().unwrap() = thread_rng_shuffler();
        }
    }

    // Tests the scenario where the resolver returns an update with multiple
    // endpoints and LB config with shuffle addresses enabled. We override the
    // shuffler functionality to reverse the order of the endpoints. The LB policy
    // should create subchannels for all addresses, and attempt to connect to them
    // in order, until a connection succeeds, at which point it should move to READY
    // state with a picker that returns that subchannel.
    #[tokio::test]
    async fn pickfirst_with_multiple_backends_shuffle_addresses() {
        let _guard = ShufflerResetGuard {};
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();
        *SHUFFLE_ENDPOINTS_FN.lock().unwrap() = test_reverse_shuffler();

        let endpoint1 = create_endpoint_with_one_address("1.1.1.1:1".to_string());
        let endpoint2 = create_endpoint_with_one_address("2.2.2.2:2".to_string());
        send_resolver_update_with_lb_config_to_policy(
            lb_policy,
            vec![endpoint1.clone(), endpoint2.clone()],
            tcc,
        );

        let subchannels = verify_subchannel_creation_from_policy(
            &mut rx_events,
            endpoint2
                .addresses
                .clone()
                .into_iter()
                .chain(endpoint1.addresses.iter().cloned())
                .collect(),
        )
        .await;

        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);

        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);

        verify_connecting_picker_from_policy(&mut rx_events).await;

        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);

        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and the LB policy successfully connects to first one and moves to
    // READY. The resolver then returns an update with a new address list that does
    // not contain the address of the currently connected subchannel. The LB policy
    // should create subchannels for the new addresses, and then realize that the
    // currently connected subchannel is not in the new address list. It should then
    // move to IDLE state and return a picker that queues RPCs. When an RPC is made,
    // the LB policy should create subchannels for the addresses specified in the
    // previous update and start connecting to them.
    #[tokio::test]
    async fn pickfirst_resolver_update_with_completely_new_address_list() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        let endpoints = create_endpoint_with_one_address("3.3.3.3:3".to_string());
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        let picker = verify_idle_picker_from_policy(&mut rx_events).await;
        verify_resolution_request(&mut rx_events).await;
        let req = test_utils::new_request();
        assert!(picker.pick(&req) == PickResult::Queue);
        verify_schedule_work_from_policy(&mut rx_events).await;
        lb_policy.work(tcc);

        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and the LB policy successfully connects to first one and moves to
    // READY. The resolver then returns an update with a new address list that
    // contains the address of the currently connected subchannel. The LB policy
    // should create subchannels for the new addresses, and then see that the
    // currently connected subchannel is in the new address list. It should then
    // send a new READY picker that returns the currently connected subchannel.
    #[tokio::test]
    async fn pickfirst_resolver_update_contains_currently_ready_subchannel() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        let mut endpoints = create_endpoint_with_n_addresses(4);
        endpoints.addresses.reverse();
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        lb_policy.subchannel_update(subchannels[0].clone(), &SubchannelState::default(), tcc);
        lb_policy.subchannel_update(subchannels[1].clone(), &SubchannelState::default(), tcc);
        lb_policy.subchannel_update(subchannels[2].clone(), &SubchannelState::default(), tcc);
        lb_policy.subchannel_update(
            subchannels[3].clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                ..Default::default()
            },
            tcc,
        );
        verify_ready_picker_from_policy(&mut rx_events, subchannels[3].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and the LB policy successfully connects to first one and moves to
    // READY. The resolver then returns an update with a an address list that is
    // identical to the first update. The LB policy should create subchannels for
    // the new addresses, and then see that the currently connected subchannel is in
    // the new address list. It should then send a new READY picker that returns the
    // currently connected subchannel.
    #[tokio::test]
    async fn pickfirst_resolver_update_contains_identical_address_list() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        lb_policy.subchannel_update(
            subchannels[0].clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                ..Default::default()
            },
            tcc,
        );
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and the LB policy successfully connects to first one and moves to
    // READY. The resolver then returns an update with a new address list that
    // removes the address of the currently connected subchannel. The LB policy
    // should create subchannels for the new addresses, and then see that the
    // currently connected subchannel is not in the new address list. It should then
    // move to IDLE state and return a picker that queues RPCs. When an RPC is made,
    // the LB policy should create subchannels for the addresses specified in the
    // previous update and start connecting to them. The test repeats this scenario
    // multiple times, each time removing the first address from the address list,
    // eventually ending up with an empty address list. The LB policy should move to
    // TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn pickfirst_resolver_update_removes_connected_address() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let mut endpoints = create_endpoint_with_n_addresses(3);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        // Address list now contains two addresses.
        endpoints.addresses.remove(0);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        let picker = verify_idle_picker_from_policy(&mut rx_events).await;
        verify_resolution_request(&mut rx_events).await;
        let req = test_utils::new_request();
        assert!(picker.pick(&req) == PickResult::Queue);
        verify_schedule_work_from_policy(&mut rx_events).await;
        lb_policy.work(tcc);

        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        // Address list now contains one address.
        endpoints.addresses.remove(0);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        let picker = verify_idle_picker_from_policy(&mut rx_events).await;
        verify_resolution_request(&mut rx_events).await;
        let req = test_utils::new_request();
        assert!(picker.pick(&req) == PickResult::Queue);
        verify_schedule_work_from_policy(&mut rx_events).await;
        lb_policy.work(tcc);

        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        // Address list is now empty.
        endpoints.addresses.remove(0);
        let update = ResolverUpdate {
            endpoints: Ok(vec![endpoints]),
            ..Default::default()
        };
        assert!(lb_policy.resolver_update(update, None, tcc).is_err());
        verify_transient_failure_picker_from_policy(
            &mut rx_events,
            "received empty address list from the name resolver".to_string(),
        )
        .await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and the LB policy successfully connects to first one and moves to
    // READY. The connected subchannel then goes down and the LB policy moves to IDLE
    // state with a picker that queues RPCs. When an RPC is made, the LB policy
    // creates subchannels for the addresses specified in the previous update and
    // starts connecting to them. The LB policy should then move to READY state with
    // a picker that returns the second subchannel.
    #[tokio::test]
    async fn pickfirst_connected_subchannel_goes_down() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_idle(lb_policy, subchannels[0].clone(), tcc);
        let picker = verify_idle_picker_from_policy(&mut rx_events).await;
        verify_resolution_request(&mut rx_events).await;
        let req = test_utils::new_request();
        assert!(picker.pick(&req) == PickResult::Queue);
        verify_schedule_work_from_policy(&mut rx_events).await;
        lb_policy.work(tcc);

        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[0].clone(),
            "connection error",
            tcc,
        );
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        move_subchannel_to_ready(lb_policy, subchannels[1].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[1].clone()).await;
    }

    // Tests the scenario where the resolver returns an update with multiple
    // addresses and the LB policy successfully connects to first one and moves to
    // READY. The connected subchannel then goes down and the LB policy moves to IDLE
    // state with a picker that queues RPCs. When an RPC is made, the LB policy
    // creates subchannels for the addresses specified in the previous update and
    // starts connecting to them. All subchannels fail to connect and the LB policy
    // moves to TRANSIENT_FAILURE state with a failing picker.
    #[tokio::test]
    async fn pickfirst_all_subchannels_goes_down() {
        let (mut rx_events, mut lb_policy, mut tcc) = setup();
        let lb_policy = lb_policy.as_mut();
        let tcc = tcc.as_mut();

        let endpoints = create_endpoint_with_n_addresses(2);
        send_resolver_update_to_policy(lb_policy, vec![endpoints.clone()], tcc);
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_ready(lb_policy, subchannels[0].clone(), tcc);
        verify_ready_picker_from_policy(&mut rx_events, subchannels[0].clone()).await;

        move_subchannel_to_idle(lb_policy, subchannels[0].clone(), tcc);
        let picker = verify_idle_picker_from_policy(&mut rx_events).await;
        verify_resolution_request(&mut rx_events).await;
        let req = test_utils::new_request();
        assert!(picker.pick(&req) == PickResult::Queue);
        verify_schedule_work_from_policy(&mut rx_events).await;
        lb_policy.work(tcc);

        let connection_error = String::from("test connection error 2");
        let subchannels =
            verify_subchannel_creation_from_policy(&mut rx_events, endpoints.addresses.clone())
                .await;
        send_initial_subchannel_updates_to_policy(lb_policy, &subchannels, tcc);
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[0].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[0].clone(), tcc);
        verify_connecting_picker_from_policy(&mut rx_events).await;
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[0].clone(),
            &connection_error,
            tcc,
        );
        verify_connection_attempt_from_policy(&mut rx_events, subchannels[1].clone()).await;
        move_subchannel_to_connecting(lb_policy, subchannels[1].clone(), tcc);
        move_subchannel_to_transient_failure(
            lb_policy,
            subchannels[1].clone(),
            &connection_error,
            tcc,
        );
        verify_transient_failure_picker_from_policy(&mut rx_events, connection_error).await;
    }
}
