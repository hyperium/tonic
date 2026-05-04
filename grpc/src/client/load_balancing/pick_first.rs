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

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Arc;

use rand::seq::SliceRandom;
use std::sync::atomic::{AtomicBool, Ordering};
use tonic::metadata::MetadataMap;

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::FailingPicker;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::ParsedJsonLbConfig;
use crate::client::load_balancing::Pick;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::QueuingPicker;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::name_resolution::Address;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::RequestHeaders;
use crate::rt::BoxedTaskHandle;
use crate::rt::GrpcRuntime;

pub(crate) static POLICY_NAME: &str = "pick_first";

type ShufflerFn = dyn Fn(&mut [Endpoint]) + Send + Sync + 'static;

#[derive(Debug, serde::Deserialize, Clone)]
pub struct PickFirstConfig {
    #[serde(rename = "shuffleAddressList")]
    pub shuffle_address_list: bool,
}

#[derive(Debug)]
pub(crate) struct PickFirstBuilder {}

impl LbPolicyBuilder for PickFirstBuilder {
    type LbPolicy = PickFirstPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        PickFirstPolicy {
            work_scheduler: options.work_scheduler,
            runtime: options.runtime,
            connectivity_state: ConnectivityState::Connecting,
            subchannels: Vec::default(),
            subchannel_states: HashMap::default(),
            selected: None,
            frontier_index: 0,
            last_resolver_error: None,
            last_connection_error: None,
            shuffler: build_shuffler(),
            timer_expired: Arc::new(AtomicBool::new(false)),
            timer_handle: None,
            steady_state: None,
        }
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }

    fn parse_config(&self, config: &ParsedJsonLbConfig) -> Result<Option<PickFirstConfig>, String> {
        let config: PickFirstConfig = config.convert_to().map_err(|e| e.to_string())?;
        Ok(Some(config))
    }
}

pub(crate) fn reg() {
    super::GLOBAL_LB_REGISTRY.add_builder(PickFirstBuilder {})
}

pub(crate) struct PickFirstPolicy {
    work_scheduler: Arc<dyn WorkScheduler>,
    runtime: GrpcRuntime,
    connectivity_state: ConnectivityState,

    // Subchannel information.
    subchannels: Vec<Arc<dyn Subchannel>>,
    subchannel_states: HashMap<Address, SubchannelState>, // Cached states for all subchannels by address.
    selected: Option<Arc<dyn Subchannel>>,
    frontier_index: usize,

    // Detailed error tracking.
    last_resolver_error: Option<String>,
    last_connection_error: Option<String>,

    // Injectable shuffler for deterministic testing.
    shuffler: Arc<ShufflerFn>,

    // Timer state tracks when the last connect attempt was started.
    timer_expired: Arc<AtomicBool>,
    timer_handle: Option<BoxedTaskHandle>,

    // Steady state tracking for continuous retries after pass exhaustion.
    steady_state: Option<SteadyState>,
}

impl Debug for PickFirstPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickFirstPolicy")
            .field("subchannels", &self.subchannels)
            .field("selected", &self.selected)
            .field("frontier_index", &self.frontier_index)
            .field("connectivity_state", &self.connectivity_state)
            .field("last_resolver_error", &self.last_resolver_error)
            .field("last_connection_error", &self.last_connection_error)
            .finish()
    }
}

impl PickFirstPolicy {
    fn rebuild_subchannels(
        &mut self,
        new_addresses: Vec<Address>,
        channel_controller: &mut dyn ChannelController,
    ) {
        // Map existing subchannels by address.
        let mut existing_subchannels: HashMap<Address, Arc<dyn Subchannel>> = self
            .subchannels
            .drain(..)
            .map(|sc| (sc.address(), sc))
            .collect();

        let mut new_states = HashMap::new();

        self.subchannels = new_addresses
            .into_iter()
            .map(|addr| {
                let subchannel = if let Some(sc) = existing_subchannels.remove(&addr) {
                    sc // Reuse existing subchannel.
                } else {
                    // New subchannel for new address.
                    let (sc, state) = channel_controller.new_subchannel(&addr);
                    self.subchannel_states.insert(addr.clone(), state);
                    sc
                };

                let state = self.subchannel_states.get(&addr).unwrap().clone();
                new_states.insert(addr.clone(), state);
                subchannel
            })
            .collect();

        self.subchannel_states = new_states; // Prune old addresses.
    }

    /// Starts a connection pass through the address list.
    // This clears the selected subchannel.
    fn start_connection_pass(&mut self, channel_controller: &mut dyn ChannelController) {
        self.selected = None;

        // If there is a viable subchannel at the frontier, connect to it and update picker to CONNECTING.
        if let Some(sc) = self.advance_frontier(true) {
            let sc = sc.clone(); // Clone to avoid borrow issues.
            self.trigger_subchannel_connection(sc, channel_controller);

            channel_controller.update_picker(LbState {
                connectivity_state: ConnectivityState::Connecting,
                picker: Arc::new(QueuingPicker {}),
            });
        } else {
            // Otherwise all addresses are in transient failure: update picker and request re-resolution.
            let error = self
                .last_connection_error
                .clone()
                .unwrap_or_else(|| "all addresses in transient failure".to_string());

            // This transition triggers a FailingPicker and requests re-resolution.
            _ = self.set_transient_failure(channel_controller, error);
        }
    }

    /// Advances the frontier to the next non-TransientFailure subchannel and returns it.
    /// If `reset` is true, starts the scan from index 0.
    // The frontier is the latest index in which connectivity has been attempted.
    fn advance_frontier(&mut self, reset: bool) -> Option<&Arc<dyn Subchannel>> {
        if reset {
            self.frontier_index = 0;
        } else {
            self.frontier_index += 1;
        }

        while self.frontier_index < self.subchannels.len() {
            let sc = &self.subchannels[self.frontier_index];
            let addr = sc.address();
            let state = self
                .subchannel_states
                .get(&addr)
                .map(|s| s.connectivity_state);

            if state == Some(ConnectivityState::TransientFailure) {
                self.frontier_index += 1;
            } else {
                return Some(sc);
            }
        }
        None
    }

    /// Triggers a connection on the subchannel, and starts the 250ms timer.
    /// If no connection succeeds before the timer expires, the frontier will advance to the next subchannel.
    fn trigger_subchannel_connection(
        &mut self,
        sc: Arc<dyn Subchannel>,
        channel_controller: &mut dyn ChannelController,
    ) {
        let sc_clone = sc.clone();
        self.connectivity_state = ConnectivityState::Connecting;
        sc_clone.connect();

        // Cancel any existing timer
        if let Some(handle) = self.timer_handle.take() {
            handle.abort();
        }

        // Start 250ms timer
        let timer_expired = self.timer_expired.clone();
        let work_scheduler = self.work_scheduler.clone();

        let sleep_fut = self.runtime.sleep(std::time::Duration::from_millis(250));
        let handle = self.runtime.spawn(Box::pin(async move {
            sleep_fut.await;
            timer_expired.store(true, Ordering::SeqCst);
            work_scheduler.schedule_work();
        }));
        self.timer_handle = Some(handle);
    }O

    // Converts the update endpoints to an address list.
    // Shuffles endpoints (if enabled) before flattening and de-duplication.
    fn compile_address(
        &mut self,
        endpoints: Vec<Endpoint>,
        config: Option<&PickFirstConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<Vec<Address>, String> {
        let mut endpoints = endpoints;

        // Shuffle endpoints if enabled.
        if config.map_or(false, |c| c.shuffle_address_list) {
            (self.shuffler)(&mut endpoints);
        }

        // Flatten and de-duplicate unique addresses in order.
        let mut seen = HashSet::new();
        let unique_addresses: Vec<Address> = endpoints
            .into_iter()
            .flat_map(|ep| ep.addresses)
            .filter(|addr| seen.insert(addr.clone()))
            .collect();

        // Partition by family (Basic IPv6 detection via colon).
        let (ipv6, ipv4): (Vec<Address>, Vec<Address>) = unique_addresses
            .into_iter()
            .partition(|addr| addr.address.contains(':'));

        // Interleave the two lists so ipv6 and ipv4 addresses are alternated.
        let mut interleaved = Vec::with_capacity(ipv6.len() + ipv4.len());
        let mut v6_iter = ipv6.into_iter();
        let mut v4_iter = ipv4.into_iter();

        loop {
            match (v6_iter.next(), v4_iter.next()) {
                (Some(v6), Some(v4)) => {
                    interleaved.push(v6);
                    interleaved.push(v4);
                }
                (Some(v6), None) => {
                    interleaved.push(v6);
                    interleaved.extend(v6_iter);
                    break;
                }
                (None, Some(v4)) => {
                    interleaved.push(v4);
                    interleaved.extend(v4_iter);
                    break;
                }
                (None, None) => break,
            }
        }

        // If we have no addresses, clear subchannels and set TRANSIENT_FAILURE.
        if interleaved.is_empty() {
            self.subchannels.clear();
            self.selected = None;
            let error = self
                .last_resolver_error
                .clone()
                .unwrap_or_else(|| "empty address list".to_string());
            return self
                .set_transient_failure(channel_controller, error)
                .map(|_| vec![]);
        }

        Ok(interleaved)
    }

    // Sets state to TRANSIENT_FAILURE and updates picker with error. Triggers a re-resolution request.
    fn set_transient_failure(
        &mut self,
        channel_controller: &mut dyn ChannelController,
        error: String,
    ) -> Result<(), String> {
        self.connectivity_state = ConnectivityState::TransientFailure;
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::TransientFailure,
            picker: Arc::new(FailingPicker {
                error: error.clone(),
            }),
        });
        channel_controller.request_resolution();
        Err(error)
    }

    // Returns true if the currently selected subchannel's address is still present in the new address list.
    fn sticky(&self, new_addresses: &[Address]) -> bool {
        self.selected
            .as_ref()
            .map(|sc| new_addresses.contains(&sc.address()))
            .unwrap_or(false)
    }
}

impl LbPolicy for PickFirstPolicy {
    type LbConfig = PickFirstConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&Self::LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        // Reset steady state on new update
        self.steady_state = None;

        match update.endpoints {
            Ok(endpoints) => {
                let new_addresses = self.compile_address(endpoints, config, channel_controller)?;
                let sticky = self.sticky(&new_addresses);
                self.rebuild_subchannels(new_addresses, channel_controller);
                if !sticky {
                    self.start_connection_pass(channel_controller);
                }
            }
            Err(e) => {
                let error = e.to_string();
                self.last_resolver_error = Some(error.clone());
                if self.subchannels.is_empty()
                    || self.connectivity_state == ConnectivityState::TransientFailure
                {
                    self.set_transient_failure(channel_controller, error)?;
                }
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
        // Update the cache for all updates.
        self.subchannel_states
            .insert(subchannel.address(), state.clone());

        // If we have a selected subchannel, only care about updates from it.
        if let Some(ref selected) = self.selected {
            if selected.address() == subchannel.address() {
                if state.connectivity_state != ConnectivityState::Ready {
                    // Lost connection: Go IDLE.
                    self.selected = None;
                    self.connectivity_state = ConnectivityState::Idle;
                    channel_controller.update_picker(LbState {
                        connectivity_state: ConnectivityState::Idle,
                        picker: Arc::new(IdlePicker {
                            work_scheduler: self.work_scheduler.clone(),
                        }),
                    });
                }
                return;
            }
        }

        // Steady State Retries: Automatically connect when a subchannel goes IDLE.
        if self.steady_state.is_some() && state.connectivity_state == ConnectivityState::Idle {
            subchannel.connect();
        }

        // Steady State Failures: Count failures across all subchannels.
        if let Some(ref mut ss) = self.steady_state {
            if state.connectivity_state == ConnectivityState::TransientFailure {
                if ss.record_failure(self.subchannels.len()) {
                    channel_controller.request_resolution();
                }
            }
        }

        // If any subchannel in the current pass reports READY, select it.
        if state.connectivity_state == ConnectivityState::Ready {
            // Check if this subchannel is part of the current pass (index <= frontier_index)
            let is_in_pass = self
                .subchannels
                .iter()
                .take(self.frontier_index + 1)
                .any(|sc| sc.address() == subchannel.address());

            if is_in_pass {
                self.selected = Some(subchannel.clone());
                self.connectivity_state = ConnectivityState::Ready;

                // Keep only the successful subchannel in the list
                self.subchannels = vec![subchannel.clone()];

                // Cancel timer
                if let Some(handle) = self.timer_handle.take() {
                    handle.abort();
                }

                channel_controller.update_picker(LbState {
                    connectivity_state: ConnectivityState::Ready,
                    picker: Arc::new(OneSubchannelPicker { sc: subchannel }),
                });
                return;
            }
        }

        // Failover on Failure: Only if the subchannel at the exact frontier_index fails.
        if let Some(attempting) = self.subchannels.get(self.frontier_index) {
            if attempting.address() == subchannel.address() {
                if state.connectivity_state == ConnectivityState::TransientFailure {
                    // Move to next address via advance_frontier
                    if let Some(next_sc) = self.advance_frontier(false) {
                        let next_sc = next_sc.clone(); // Clone to avoid borrow issues
                        self.trigger_subchannel_connection(next_sc, channel_controller);
                    } else {
                        // Exhausted: TRANSIENT_FAILURE and request re-resolution.
                        let error = state
                            .last_connection_error
                            .as_ref()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "all addresses failed".to_string());

                        self.last_connection_error = Some(error.clone());
                        _ = self.set_transient_failure(channel_controller, error);

                        // Transition to steady state mode
                        self.steady_state = Some(SteadyState::new());
                    }
                }
            }
        }
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        if self.connectivity_state == ConnectivityState::Idle {
            self.exit_idle(channel_controller);
        } else if self.connectivity_state == ConnectivityState::Connecting {
            // Check if timer expired
            if self.timer_expired.load(Ordering::SeqCst) {
                self.timer_expired.store(false, Ordering::SeqCst); // Reset

                // Advance frontier and trigger next connection.
                if let Some(next_sc) = self.advance_frontier(false) {
                    let next_sc = next_sc.clone();
                    self.trigger_subchannel_connection(next_sc, channel_controller);
                }
            }
        }
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.start_connection_pass(channel_controller);
    }
}

#[derive(Debug)]
struct OneSubchannelPicker {
    sc: Arc<dyn Subchannel>,
}

impl Picker for OneSubchannelPicker {
    fn pick(&self, _: &RequestHeaders) -> PickResult {
        PickResult::Pick(Pick {
            subchannel: self.sc.clone(),
            metadata: MetadataMap::new(),
            on_complete: None,
        })
    }
}

#[derive(Debug)]
struct IdlePicker {
    work_scheduler: Arc<dyn WorkScheduler>,
}

impl Picker for IdlePicker {
    fn pick(&self, _: &RequestHeaders) -> PickResult {
        self.work_scheduler.schedule_work();
        PickResult::Queue
    }
}

fn build_shuffler() -> Arc<ShufflerFn> {
    Arc::new(|endpoints| {
        let mut rng = rand::rng();
        endpoints.shuffle(&mut rng);
    })
}

#[derive(Debug)]
struct SteadyState {
    /// The number of failures connecting, used to roughly approximate if a re-resolution needs to happen.
    failure_count: usize,
}

impl SteadyState {
    fn new() -> Self {
        Self { failure_count: 0 }
    }

    /// Increments the failure count and returns true if re-resolution is required.
    fn record_failure(&mut self, subchannels_len: usize) -> bool {
        self.failure_count += 1;
        if self.failure_count >= subchannels_len {
            self.failure_count = 0;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::load_balancing::test_utils::{
        TestChannelController, TestEvent, TestWorkScheduler,
    };
    use std::sync::mpsc;
    use std::time::Duration;

    fn setup() -> (
        mpsc::Receiver<TestEvent>,
        PickFirstPolicy,
        Box<TestChannelController>,
    ) {
        let (tx, rx) = mpsc::channel();
        let work_scheduler = Arc::new(TestWorkScheduler {
            tx_events: tx.clone(),
        });
        let runtime = crate::rt::default_runtime();
        let mut policy = PickFirstBuilder {}.build(LbPolicyOptions {
            work_scheduler,
            runtime,
        });

        // Deterministic shuffling for tests: reverse the endpoints
        policy.shuffler = Arc::new(|endpoints| {
            endpoints.reverse();
        });

        let controller = Box::new(TestChannelController { tx_events: tx });
        (rx, policy, controller)
    }

    fn create_endpoints(addrs: Vec<&str>) -> Vec<Endpoint> {
        addrs
            .into_iter()
            .map(|a| Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from(a.to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect()
    }

    #[tokio::test]
    async fn test_pick_first_basic_connection() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1", "addr2"]);
        let update = ResolverUpdate {
            endpoints: Ok(endpoints),
            ..Default::default()
        };

        policy
            .resolver_update(update, None, controller.as_mut())
            .unwrap();

        // Expect NewSubchannel x2, Connect, UpdatePicker(Connecting)
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Simulating READY for addr1
        let sc1 = policy.subchannels[0].clone();
        policy.subchannel_update(
            sc1.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Should update picker to READY with sc1
        match rx.recv().unwrap() {
            TestEvent::UpdatePicker(state) => {
                assert_eq!(state.connectivity_state, ConnectivityState::Ready);
                let res = state.picker.pick(&RequestHeaders::default());
                match res {
                    PickResult::Pick(pick) => {
                        assert_eq!(pick.subchannel.address().address.to_string(), "addr1")
                    }
                    other => panic!("unexpected pick result {:?}", other),
                }
            }
            other => panic!("unexpected event {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pick_first_failover() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1", "addr2"]);
        policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints),
                    ..Default::default()
                },
                None,
                controller.as_mut(),
            )
            .unwrap();

        // Expect NewSubchannel x2, Connect, UpdatePicker(Connecting)
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Simulate addr1 failing
        let sc1 = policy.subchannels[0].clone();
        policy.subchannel_update(
            sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Should connect to addr2
        match rx.recv().unwrap() {
            TestEvent::Connect(addr) => assert_eq!(addr.address.to_string(), "addr2"),
            other => panic!("unexpected event {:?}", other),
        }

        // Simulate addr2 succeeding
        let sc2 = policy.subchannels[1].clone();
        policy.subchannel_update(
            sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        match rx.recv().unwrap() {
            TestEvent::UpdatePicker(state) => {
                assert_eq!(state.connectivity_state, ConnectivityState::Ready)
            }
            other => panic!("unexpected event {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pick_first_stickiness() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1", "addr2"]);
        policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints),
                    ..Default::default()
                },
                None,
                controller.as_mut(),
            )
            .unwrap();

        // Expect NewSubchannel x2, Connect, UpdatePicker(Connecting)
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Make addr1 READY
        let sc1 = policy.subchannels[0].clone();
        policy.subchannel_update(
            sc1.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Expect UpdatePicker(Ready)
        match rx.recv().unwrap() {
            TestEvent::UpdatePicker(state) => {
                assert_eq!(state.connectivity_state, ConnectivityState::Ready)
            }
            other => panic!("unexpected event {:?}", other),
        }

        // New resolver update including addr1
        let endpoints_new = create_endpoints(vec!["addr2", "addr1", "addr3"]);
        policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints_new),
                    ..Default::default()
                },
                None,
                controller.as_mut(),
            )
            .unwrap();

        // Should create subchannel for addr2 (was cleared by cleanup) and addr3 (new)
        match rx.recv().unwrap() {
            TestEvent::NewSubchannel(sc) => assert_eq!(sc.address().address.to_string(), "addr2"),
            other => panic!("unexpected event {:?}", other),
        }
        match rx.recv().unwrap() {
            TestEvent::NewSubchannel(sc) => assert_eq!(sc.address().address.to_string(), "addr3"),
            other => panic!("unexpected event {:?}", other),
        }

        // Should NOT have any more events (no Connect, no UpdatePicker) because it is sticky
        std::thread::sleep(Duration::from_millis(50));
        assert!(rx.try_recv().is_err(), "unexpected event");

        assert_eq!(
            policy
                .selected
                .as_ref()
                .unwrap()
                .address()
                .address
                .to_string(),
            "addr1"
        );
    }

    #[tokio::test]
    async fn test_pick_first_exhaustion() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1"]);
        policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints),
                    ..Default::default()
                },
                None,
                controller.as_mut(),
            )
            .unwrap();

        // Expect NewSubchannel, Connect, UpdatePicker(Connecting)
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Simulate addr1 failure
        let sc1 = policy.subchannels[0].clone();
        policy.subchannel_update(
            sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Should update picker to TransientFailure
        match rx.recv().unwrap() {
            TestEvent::UpdatePicker(state) => assert_eq!(
                state.connectivity_state,
                ConnectivityState::TransientFailure
            ),
            other => panic!("unexpected event {:?}", other),
        }

        // Should request re-resolution
        match rx.recv().unwrap() {
            TestEvent::RequestResolution => {}
            other => panic!("unexpected event {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pick_first_shuffling_and_interleaving_deterministic() {
        let (_rx, mut policy, mut controller) = setup();

        // Enable shuffling in config
        let config = PickFirstConfig {
            shuffle_address_list: true,
        };

        // Provide three endpoints:
        // EP1: [V6_1, V4_1]
        // EP2: [V6_2]
        // EP3: [V4_2]
        let endpoints = vec![
            Endpoint {
                addresses: vec![
                    Address {
                        address: crate::byte_str::ByteStr::from("::1".to_string()),
                        ..Default::default()
                    },
                    Address {
                        address: crate::byte_str::ByteStr::from("127.0.0.1".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from("::2".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from("127.0.0.2".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];

        policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints),
                    ..Default::default()
                },
                Some(&config),
                controller.as_mut(),
            )
            .unwrap();

        let resulting_addrs: Vec<String> = policy
            .subchannels
            .iter()
            .map(|sc| sc.address().address.to_string())
            .collect();

        // Mock shuffler reverses endpoints: [EP3, EP2, EP1]
        // EP3: [127.0.0.2] (V4)
        // EP2: [::2] (V6)
        // EP1: [::1] (V6), [127.0.0.1] (V4)
        //
        // Categorized:
        // IPv6: [::2, ::1]
        // IPv4: [127.0.0.2, 127.0.0.1]
        //
        // Interleaved: [::2, 127.0.0.2, ::1, 127.0.0.1]
        let expected = vec!["::2", "127.0.0.2", "::1", "127.0.0.1"];
        assert_eq!(
            resulting_addrs, expected,
            "Interleaving or shuffling failed"
        );
    }

    #[tokio::test]
    async fn test_pick_first_duplicate_de_duplication() {
        let (rx, mut policy, mut controller) = setup();

        // Create endpoints with duplicates
        let endpoints = vec![
            Endpoint {
                addresses: vec![
                    Address {
                        address: crate::byte_str::ByteStr::from("addr1".to_string()),
                        ..Default::default()
                    },
                    Address {
                        address: crate::byte_str::ByteStr::from("addr1".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![
                    Address {
                        address: crate::byte_str::ByteStr::from("addr2".to_string()),
                        ..Default::default()
                    },
                    Address {
                        address: crate::byte_str::ByteStr::from("addr1".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ];

        policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints),
                    ..Default::default()
                },
                None,
                controller.as_mut(),
            )
            .unwrap();

        // Should only create subchannels for addr1 and addr2 (2 unique addresses)
        rx.recv().unwrap(); // NewSubchannel(addr1)
        rx.recv().unwrap(); // NewSubchannel(addr2)

        // Verify no 3rd subchannel was created
        std::thread::sleep(Duration::from_millis(50));
        while let Ok(event) = rx.try_recv() {
            if let TestEvent::NewSubchannel(_) = event {
                panic!("Duplicate subchannel created");
            }
        }

        assert_eq!(policy.subchannels.len(), 2, "De-duplication failed");
    }

    #[tokio::test]
    async fn test_pick_first_empty_update_clears_state() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1", "addr2"]);

        // Initial update with addresses
        policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints),
                    ..Default::default()
                },
                None,
                controller.as_mut(),
            )
            .unwrap();

        assert_eq!(policy.subchannels.len(), 2);

        // Make addr1 READY so it becomes selected
        let sc1 = policy.subchannels[0].clone();
        policy.subchannel_update(
            sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
            controller.as_mut(),
        );
        assert!(policy.selected.is_some());

        // Drain events (NewSubchannel x2, Connect, UpdatePicker x2)
        while rx.try_recv().is_ok() {}

        // Send empty update
        let res = policy.resolver_update(
            ResolverUpdate {
                endpoints: Ok(vec![]),
                ..Default::default()
            },
            None,
            controller.as_mut(),
        );

        assert!(res.is_err());
        assert_eq!(policy.subchannels.len(), 0);
        assert!(policy.selected.is_none());

        // Should have updated picker to TransientFailure and requested resolution
        match rx.recv().unwrap() {
            TestEvent::UpdatePicker(state) => {
                assert_eq!(
                    state.connectivity_state,
                    ConnectivityState::TransientFailure
                );
            }
            other => panic!("unexpected event {:?}", other),
        }
        match rx.recv().unwrap() {
            TestEvent::RequestResolution => {}
            other => panic!("unexpected event {:?}", other),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_pick_first_timer_advancement() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1", "addr2"]);
        let update = ResolverUpdate {
            endpoints: Ok(endpoints),
            ..Default::default()
        };

        policy
            .resolver_update(update, None, controller.as_mut())
            .unwrap();

        // Expect NewSubchannel x2, Connect(addr1), UpdatePicker(Connecting)
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Simulate timer expiration by setting the flag directly!
        policy
            .timer_expired
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Manually call work() to process the timer expiration!
        policy.work(controller.as_mut());

        // Expect Connect event for addr2 due to timer expiration
        // Loop to check for event without blocking the thread
        let mut found = None;
        for _ in 0..10 {
            match rx.try_recv() {
                Ok(event) => {
                    found = Some(event);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Yield to runtime to allow timer task to run!
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(e) => panic!("error recv: {:?}", e),
            }
        }

        match found {
            Some(TestEvent::Connect(addr)) => assert_eq!(addr.address.to_string(), "addr2"),
            other => panic!("unexpected result {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pick_first_steady_state_retries() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1"]);
        let update = ResolverUpdate {
            endpoints: Ok(endpoints),
            ..Default::default()
        };

        policy
            .resolver_update(update, None, controller.as_mut())
            .unwrap();

        // Expect NewSubchannel, Connect(addr1), UpdatePicker(Connecting)
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Simulate addr1 failure
        let sc1 = policy.subchannels[0].clone();
        policy.subchannel_update(
            sc1.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Expect UpdatePicker(TransientFailure) and RequestResolution
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Now we are in steady state!
        assert!(policy.steady_state.is_some());

        // Simulate addr1 transitioning to IDLE (backoff over)
        policy.subchannel_update(
            sc1.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Should automatically call connect() again!
        match rx.recv().unwrap() {
            TestEvent::Connect(addr) => assert_eq!(addr.address.to_string(), "addr1"),
            other => panic!("unexpected event {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pick_first_steady_state_multi_backend() {
        let (rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1", "addr2"]);
        let update = ResolverUpdate {
            endpoints: Ok(endpoints),
            ..Default::default()
        };

        policy
            .resolver_update(update, None, controller.as_mut())
            .unwrap();

        // Expect NewSubchannel x2, Connect(addr1), UpdatePicker(Connecting)
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Simulate addr1 failure
        let sc1 = policy.subchannels[0].clone();
        policy.subchannel_update(
            sc1.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Should failover to addr2: expect Connect(addr2)
        match rx.recv().unwrap() {
            TestEvent::Connect(addr) => assert_eq!(addr.address.to_string(), "addr2"),
            other => panic!("unexpected event {:?}", other),
        }

        // Now while addr2 is connecting, simulate addr1 going IDLE (backoff over)
        policy.subchannel_update(
            sc1.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // We should NOT reconnect to addr1 during the first pass!
        // Wait a bit to ensure no event is sent
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(rx.try_recv().is_err(), "unexpected event");

        // Now fail addr2 to complete first pass
        let sc2 = policy.subchannels[1].clone();
        policy.subchannel_update(
            sc2.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Expect UpdatePicker(TransientFailure) and RequestResolution
        rx.recv().unwrap();
        rx.recv().unwrap();

        // Now we are in steady state!
        assert!(policy.steady_state.is_some());

        // Simulate addr1 going IDLE again
        policy.subchannel_update(
            sc1.clone(),
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
            controller.as_mut(),
        );

        // Now it SHOULD automatically call connect() again!
        match rx.recv().unwrap() {
            TestEvent::Connect(addr) => assert_eq!(addr.address.to_string(), "addr1"),
            other => panic!("unexpected event {:?}", other),
        }
    }
}
