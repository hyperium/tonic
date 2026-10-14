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
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use rand::seq::SliceRandom;

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::FailingPicker;
use crate::client::load_balancing::LbConfigJson;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::Pick;
use crate::client::load_balancing::PickOptions;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::QueuingPicker;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::load_balancing::subchannel::SubchannelUpdate;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::Address;
use crate::metadata::MetadataMap;
use crate::rt::BoxedTaskHandle;
use crate::rt::GrpcRuntime;

pub static POLICY_NAME: &str = "pick_first";

type ShufflerFn = dyn Fn(&mut [Endpoint]) + Send + Sync + 'static;

#[derive(Debug, serde::Deserialize, Clone, Default)]
pub(crate) struct PickFirstConfig {
    #[serde(default, rename = "shuffleAddressList")]
    shuffle_address_list: bool,
}

#[derive(Debug)]
pub(crate) struct PickFirstBuilder {}

impl LbPolicyBuilder for PickFirstBuilder {
    type LbPolicy = PickFirstPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        PickFirstPolicy {
            work_scheduler: options.work_scheduler,
            runtime: options.runtime,
            connectivity_state: ConnectivityState::Idle,
            subchannels: Vec::default(),
            subchannel_states: HashMap::default(),
            selected: None,
            frontier_index: 0,
            last_connection_error: None,
            shuffler: build_shuffler(),
            timer: None,
            steady_state: None,
        }
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }

    fn parse_config(&self, config: &LbConfigJson) -> Result<PickFirstConfig, String> {
        let config: PickFirstConfig = config.convert_to().map_err(|e| e.to_string())?;
        Ok(config)
    }
}

pub(crate) fn reg() {
    super::GLOBAL_LB_REGISTRY.add_builder(PickFirstBuilder {});
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
    last_connection_error: Option<String>,

    // Injectable shuffler for deterministic testing.
    shuffler: Arc<ShufflerFn>,

    // Timer state tracks when the last connect attempt was started.
    timer: Option<Timer>,

    // Steady state tracking for continuous retries after pass exhaustion.
    // TODO: should steady_state be a "mode" selector enum, e.g.:
    // - FirstPass - holds the timer above
    // - SteadyState - existing info
    // - Ready - holds selected subchannel
    // - Error - for zero addresses; holds no data
    steady_state: Option<SteadyState>,
}

impl Debug for PickFirstPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickFirstPolicy")
            .field("subchannels", &self.subchannels)
            .field("selected", &self.selected)
            .field("frontier_index", &self.frontier_index)
            .field("connectivity_state", &self.connectivity_state)
            .field("last_connection_error", &self.last_connection_error)
            .finish()
    }
}

impl PickFirstPolicy {
    fn rebuild_subchannels(
        &mut self,
        new_addresses: Vec<Address>,
        channel_controller: &mut dyn ChannelController,
    ) -> Option<Arc<dyn Subchannel>> {
        // Map existing subchannels by address.
        let mut existing_subchannels: HashMap<Address, Arc<dyn Subchannel>> = self
            .subchannels
            .drain(..)
            .map(|sc| (sc.address(), sc))
            .collect();

        let mut new_subchannels = Vec::with_capacity(new_addresses.len());
        let mut new_states = HashMap::with_capacity(new_addresses.len());
        let mut ready_subchannel = None;

        for addr in new_addresses {
            let (sc, state) = if let Some(sc) = existing_subchannels.remove(&addr) {
                let state = self.subchannel_states.get(&addr).unwrap().clone();
                (sc, state)
            } else {
                // Get a new subchannel handle from the controller if we don't
                // have an existing one.  Updates for it are delivered to our
                // work method via our work scheduler.
                channel_controller.new_subchannel(&addr, self.work_scheduler.clone())
            };

            // Track the best candidate for immediate activation:
            // 1. Priority: The currently selected subchannel if still READY.
            // 2. Fallback: The first generic READY subchannel encountered.
            if state.connectivity_state == ConnectivityState::Ready {
                if self.subchannel_is_selected(&sc) {
                    // Sticky channel wins immediately and overrides any
                    // fallback candidates.
                    ready_subchannel = Some(sc.clone());
                } else if ready_subchannel.is_none() {
                    // Capture fallback candidate, but does not overwrite if a
                    //sticky channel was already found.
                    ready_subchannel = Some(sc.clone());
                }
            }

            new_subchannels.push(sc);
            new_states.insert(addr, state);
        }

        self.subchannels = new_subchannels;
        self.subchannel_states = new_states;
        ready_subchannel
    }

    /// Call when the selected subchannel loses connection.
    // This causes the LB to go IDLE.
    fn subchannel_drop(&mut self, channel_controller: &mut dyn ChannelController) {
        self.selected = None;
        self.update_picker(
            ConnectivityState::Idle,
            Arc::new(IdlePicker::new(self.work_scheduler.clone())),
            channel_controller,
        );
    }

    fn update_picker(
        &mut self,
        connectivity_state: ConnectivityState,
        picker: Arc<dyn Picker>,
        channel_controller: &mut dyn ChannelController,
    ) {
        if self.connectivity_state == connectivity_state
            && connectivity_state == ConnectivityState::Connecting
        {
            // Prevent redundant connecting updates.
            // TODO: prevent redundant IDLE updates?
            return;
        }
        self.connectivity_state = connectivity_state;
        channel_controller.update_picker(LbState {
            connectivity_state,
            picker,
        });
    }

    fn subchannel_activate(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        channel_controller: &mut dyn ChannelController,
    ) {
        if self.subchannel_is_selected(&subchannel) {
            // Already selected; skip activation.
            return;
        }
        self.selected = Some(subchannel.clone());
        self.subchannels = vec![subchannel.clone()]; // Keep only the winner.
        self.steady_state = None; // Reset mode to First Pass.
        self.timer = None; // Stop the happy eyeballs timer.

        self.update_picker(
            ConnectivityState::Ready,
            Arc::new(OneSubchannelPicker { sc: subchannel }),
            channel_controller,
        );
    }

    /// Starts a connection pass through the address list.
    // This clears the selected subchannel.
    fn start_connection_pass(&mut self, channel_controller: &mut dyn ChannelController) {
        self.selected = None;

        // If there is a viable subchannel at the frontier, connect to it and
        // update picker to CONNECTING.
        if let Some(sc) = self.advance_frontier(true) {
            self.trigger_subchannel_connection(sc, channel_controller);

            // TODO: avoid this update if we are in TF (i.e. sticky TF)?
            self.update_picker(
                ConnectivityState::Connecting,
                Arc::new(QueuingPicker {}),
                channel_controller,
            );
        } else {
            // Otherwise all addresses are in transient failure: update picker.
            // TODO: set the last connection error?  Is it correct to do so,
            // e.g. if the address it came from was removed from the address
            // list and a name resolver update triggered this connection pass?
            _ = self.set_transient_failure(channel_controller, None);
        }
    }

    // Book-keeping for tracking progress on the first pass through the address
    // list. Assumes the subchannel is in a non-READY state.
    // If the failure is from the subchannel at the frontier, advances the
    // frontier and triggers a connection on the next subchannel.
    fn update_first_pass(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        // Advance frontier if this failure is from the active frontier subchannel.
        if let Some(attempting) = self.subchannels.get(self.frontier_index)
            && attempting.address() == subchannel.address()
            && state.connectivity_state == ConnectivityState::TransientFailure
            && let Some(next_sc) = self.advance_frontier(false)
        {
            self.trigger_subchannel_connection(next_sc, channel_controller);
        }

        // Check if First Pass is fully exhausted (frontier exhausted AND zero connecting).
        if self.frontier_index >= self.subchannels.len() {
            let any_connecting = self.subchannels.iter().any(|sc| {
                self.subchannel_states
                    .get(&sc.address())
                    .is_some_and(|s| s.connectivity_state == ConnectivityState::Connecting)
            });

            if !any_connecting {
                // Nothing currently connecting; first pass complete.  Enter
                // steady state.
                self.timer = None;
                let error = self.last_connection_error.clone();
                _ = self.set_transient_failure(channel_controller, error);
                self.steady_state = Some(SteadyState::new(self.subchannels.len()));

                // Trigger connection attempts on any subchannels that
                // transitioned to IDLE during the first pass, ensuring they
                // don't get stuck.
                for sc in &self.subchannels {
                    let is_idle = self
                        .subchannel_states
                        .get(&sc.address())
                        .is_some_and(|s| s.connectivity_state == ConnectivityState::Idle);
                    if is_idle {
                        sc.connect();
                    }
                }
            }
        }
    }

    /// Advances the frontier to the next non-TransientFailure subchannel and
    /// returns it. If `reset` is true, starts the scan from index 0.
    // The frontier is the latest index in which connectivity has been attempted.
    // Returns a cloned Subchannel to avoid borrowing from self.
    fn advance_frontier(&mut self, reset: bool) -> Option<Arc<dyn Subchannel>> {
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
                .map(|s| s.connectivity_state)
                .expect("Expected non-None subchannel state");

            match state {
                // Push the frontier if sc is in TransientFailure
                ConnectivityState::TransientFailure => self.frontier_index += 1,
                // Otherwise return the subchannel.
                _ => return Some(sc.clone()),
            }
        }
        None
    }

    /// Returns true if the given subchannel matches the currently selected
    /// active subchannel.
    fn subchannel_is_selected(&self, subchannel: &Arc<dyn Subchannel>) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|sel| sel.address() == subchannel.address())
    }

    /// Returns true if the subchannel's address is present in the most recently
    /// received address list.
    // This compares against the current list of subchannels the LB is
    // attempting to connect to. To see if the LB already connected to the
    // channel, use 'subchannel_is_selected'.
    fn subchannel_is_current(&self, subchannel: &Arc<dyn Subchannel>) -> bool {
        self.subchannels
            .iter()
            .any(|sc| sc.address() == subchannel.address())
    }

    /// Triggers a connection on the subchannel, and starts the 250ms timer.
    /// If no connection succeeds before the timer expires, the frontier will
    /// advance to the next subchannel.
    fn trigger_subchannel_connection(
        &mut self,
        sc: Arc<dyn Subchannel>,
        channel_controller: &mut dyn ChannelController,
    ) {
        self.subchannel_states.insert(
            sc.address(),
            SubchannelState {
                connectivity_state: ConnectivityState::Connecting,
                last_connection_error: None,
            },
        );
        sc.connect();

        // Start happy eyeballs timer; replacing any pre-existing timer.
        self.timer = Some(Timer::start(
            self.runtime.clone(),
            self.work_scheduler.clone(),
        ));
    }

    // Converts the update endpoints to an address list.
    // Shuffles endpoints (if enabled) before flattening and de-duplication.
    fn compile_address(
        &mut self,
        mut endpoints: Vec<Endpoint>,
        config: &PickFirstConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Vec<Address> {
        // Shuffle endpoints if enabled.
        if config.shuffle_address_list {
            (self.shuffler)(&mut endpoints);
        }

        // Flatten and de-duplicate unique addresses in order.
        let mut seen = HashSet::new();
        let unique_addresses: Vec<Address> = endpoints
            .into_iter()
            .flat_map(|ep| ep.addresses)
            .filter(|addr| seen.insert(addr.clone()))
            .collect();

        // Partition out all 'unknown' non-TCP addresses.
        // This is to remain consistent with similar behavior in C++ and Java.
        let (tcp_addresses, unknown): (Vec<Address>, Vec<Address>) =
            unique_addresses.into_iter().partition(|addr| {
                addr.network_type == crate::client::name_resolution::TCP_IP_NETWORK_TYPE
            });

        // Partition by family (Basic IPv6 detection via colon).
        let (ipv6, ipv4): (Vec<Address>, Vec<Address>) = tcp_addresses
            .into_iter()
            .partition(|addr| addr.address.contains(':'));

        // Interleave the two lists so ipv6 and ipv4 addresses are alternated.
        let mut interleaved = Vec::with_capacity(ipv6.len() + ipv4.len() + unknown.len());
        let mut v6_iter = ipv6.into_iter();
        let mut v4_iter = ipv4.into_iter();
        let mut unknown_iter = unknown.into_iter();

        loop {
            let mut more = false;

            if let Some(v6) = v6_iter.next() {
                interleaved.push(v6);
                more = true;
            }
            if let Some(v4) = v4_iter.next() {
                interleaved.push(v4);
                more = true;
            }
            if let Some(unknown) = unknown_iter.next() {
                interleaved.push(unknown);
                more = true;
            }

            if !more {
                break;
            }
        }
        interleaved
    }

    // Sets LB state to TRANSIENT_FAILURE and updates picker with error.
    // Triggers a re-resolution request.
    // TODO: make error mandatory.
    fn set_transient_failure(
        &mut self,
        channel_controller: &mut dyn ChannelController,
        error: Option<String>,
    ) -> Result<(), String> {
        // Replace the last connection error if we have a new one.
        if let Some(e) = error {
            self.last_connection_error = Some(e);
        }
        let err = self
            .last_connection_error
            .clone()
            .expect("no last connection error set");
        self.update_picker(
            ConnectivityState::TransientFailure,
            Arc::new(FailingPicker { error: err.clone() }),
            channel_controller,
        );
        channel_controller.request_resolution();
        Err(err.clone())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        if !self.subchannel_is_current(&subchannel) {
            // This update is from an outdated subchannel that is no longer in
            // the address list. Ignore it.
            return;
        }

        // Update the cache for all updates.
        self.subchannel_states
            .insert(subchannel.address(), state.clone());

        match (
            // Does the load balancer have an active subchannel already?
            self.subchannel_is_selected(&subchannel),
            // What is the updating subchannel's state?
            state.connectivity_state,
        ) {
            (true, ConnectivityState::Ready) => {
                // The selected subchannel is still ready; do nothing w/update.
            }
            (true, _) => {
                // The selected subchannel has failed (is no longer READY);
                // drop the connection.
                self.subchannel_drop(channel_controller);
            }
            (false, ConnectivityState::Ready) => {
                // The updating subchannel is READY; activate it.
                self.subchannel_activate(subchannel, channel_controller);
            }
            (false, _) => {
                // Always capture freshest unselected error.
                if state.connectivity_state == ConnectivityState::TransientFailure
                    && let Some(err) = &state.last_connection_error
                {
                    self.last_connection_error = Some(err.clone());
                }

                // Track progress based on whether we are in a connection pass.
                if let Some(steady) = self.steady_state.as_mut() {
                    steady.subchannel_nonready(channel_controller, subchannel, state);
                } else {
                    self.update_first_pass(subchannel, state, channel_controller);
                }
            }
        }
    }
}

impl LbPolicy for PickFirstPolicy {
    type LbConfig = PickFirstConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        self.timer = None;

        // Reset steady state on new update
        self.steady_state = None;

        match update.endpoints {
            Ok(endpoints) => {
                let new_addresses = self.compile_address(endpoints, config, channel_controller);
                // If we have no addresses, clear subchannels and set TRANSIENT_FAILURE.
                if new_addresses.is_empty() {
                    self.subchannels.clear();
                    self.selected = None;
                    self.set_transient_failure(
                        channel_controller,
                        Some("empty address list".to_string()),
                    )?;
                }

                if let Some(ready_subchannel) =
                    self.rebuild_subchannels(new_addresses, channel_controller)
                {
                    self.subchannel_activate(ready_subchannel, channel_controller);
                } else {
                    self.start_connection_pass(channel_controller);
                }
            }
            Err(e) => {
                let error = e.to_string();
                if self.subchannels.is_empty()
                    || self.connectivity_state == ConnectivityState::TransientFailure
                {
                    self.set_transient_failure(channel_controller, Some(error))?;
                }
            }
        }

        Ok(())
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        if let Some(data) = data {
            // If data is set, it should be a subchannel update.
            match data.downcast::<SubchannelUpdate>() {
                Ok(update) => {
                    self.subchannel_update(update.subchannel, &update.state, channel_controller);
                }
                Err(data) => debug_assert!(
                    false,
                    "work called with {data:?}; expected a SubchannelUpdate"
                ),
            }
            return;
        }
        if self.connectivity_state == ConnectivityState::Idle {
            // TODO: is it safe to assume any call to work() while idle means we
            // should connect?
            self.exit_idle(channel_controller);
        } else if self.timer.as_ref().is_some_and(|t| t.expired()) {
            // Advance frontier and trigger next connection.
            if let Some(next_sc) = self.advance_frontier(false) {
                self.trigger_subchannel_connection(next_sc, channel_controller);
            }
        }
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        self.start_connection_pass(channel_controller);
    }
}

/// Implements the happy eyeballs timer task.  `expired` becomes set when it
/// fires.  When dropped, the timer is cancelled.
struct Timer {
    expired: Arc<AtomicBool>,
    handle: BoxedTaskHandle,
}

impl Timer {
    /// Starts a new timer, returning it.
    fn start(runtime: GrpcRuntime, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        let expired = Arc::new(AtomicBool::new(false));
        let expired_clone = expired.clone();
        let handle = runtime.clone().spawn(Box::pin(async move {
            runtime.sleep(std::time::Duration::from_millis(250)).await;
            expired_clone.store(true, Ordering::SeqCst);
            work_scheduler.schedule_work(None);
        }));
        Self { expired, handle }
    }

    /// Returns whether the timer has expired yet.
    fn expired(&self) -> bool {
        self.expired.load(Ordering::SeqCst)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[derive(Debug)]
struct OneSubchannelPicker {
    sc: Arc<dyn Subchannel>,
}

impl Picker for OneSubchannelPicker {
    fn pick(&self, _: PickOptions<'_>) -> PickResult {
        PickResult::Pick(Pick {
            subchannel: self.sc.clone(),
            metadata: MetadataMap::new(),
            on_complete: None,
        })
    }
}

#[derive(Debug)]
struct IdlePicker {
    triggered_work: AtomicBool,
    work_scheduler: Arc<dyn WorkScheduler>,
}

impl IdlePicker {
    fn new(work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        Self {
            triggered_work: AtomicBool::new(false),
            work_scheduler,
        }
    }
}

impl Picker for IdlePicker {
    fn pick(&self, _: PickOptions<'_>) -> PickResult {
        if !self.triggered_work.swap(true, Ordering::Relaxed) {
            self.work_scheduler.schedule_work(None);
        }
        PickResult::Queue
    }
}

fn build_shuffler() -> Arc<ShufflerFn> {
    Arc::new(|endpoints| {
        let mut rng = rand::rng();
        endpoints.shuffle(&mut rng);
    })
}

/// Tracks a the 'steady state' pass of subchannels when looking for a ready
/// connection. If the number of reported subchannel failures reaches the
/// failure threshold, this will ask the Name Resolver to re-resolve.
#[derive(Debug)]
struct SteadyState {
    /// The number of failures before triggering a re-resolution of addresses.
    /// This is a rough heuristic to approximate if all subchannels have failed
    /// since we entered steady state, and can be tuned as needed.
    failure_threshold: usize,
    /// The number of failures connecting, used to roughly approximate if a
    /// re-resolution needs to happen.
    failure_count: usize,
}

impl SteadyState {
    fn new(threshold: usize) -> Self {
        Self {
            failure_threshold: threshold,
            failure_count: 0,
        }
    }

    /// Handles non-ready subchannel updates when the LB is in 'steady state'
    /// connection mode.
    fn subchannel_nonready(
        &mut self,
        channel_controller: &mut dyn ChannelController,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) {
        match state.connectivity_state {
            ConnectivityState::Idle => {
                // Subchannel backoff expired: trigger reconnection attempt.
                subchannel.connect();
            }
            ConnectivityState::TransientFailure => {
                // Track failures. If all known subchannels have failed,
                // request new addresses.
                self.failure_count += 1;
                if self.failure_count >= self.failure_threshold {
                    self.failure_count = 0;
                    channel_controller.request_resolution();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::call_attributes::CallAttributes;
    use crate::client::RequestHeaders;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::client::load_balancing::test_utils::TestEvent;
    use crate::rt::default_runtime;

    // Helper to create endpoints from a list of address strings.
    // If attrs are provided, they will be added to each endpoint; otherwise,
    // default attributes will be used.
    fn create_endpoints(
        addrs: Vec<&str>,
        attrs: Option<crate::attributes::Attributes>,
    ) -> Vec<Endpoint> {
        addrs
            .into_iter()
            .map(|a| Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from(a.to_string()),
                    network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                    attributes: attrs.clone().unwrap_or_default(),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect()
    }

    // Constructs the test environment for PickFirstPolicy tests.
    fn new_env() -> TestEnv<PickFirstPolicy> {
        let mut env = TestEnv::new(|work_scheduler| {
            PickFirstBuilder {}.build(LbPolicyOptions {
                work_scheduler,
                runtime: default_runtime(),
            })
        });

        // Deterministic shuffling for tests: reverse the endpoints
        env.policy.shuffler = Arc::new(|endpoints| {
            endpoints.reverse();
        });

        env
    }

    impl TestEnv<PickFirstPolicy> {
        // Helper to simulate a basic connection against a list of
        // addresses. The resulting events can be inspected via the test environment.
        // Does not imply that the connection succeeded or failed.
        fn simulate_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) {
            let addrs_len = addrs.len();
            let endpoints = create_endpoints(addrs, attrs);
            self.send_resolver_update(endpoints).unwrap();

            for _ in 0..addrs_len {
                self.expect_new_subchannel();
            }

            self.expect_connect();

            let state = self.expect_picker_update();
            assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
        }

        fn simulate_successful_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) {
            self.simulate_connection(addrs, attrs);

            // Simulating READY for addr1.
            let sc1 = self.policy.subchannels[0].clone();
            self.send_subchannel_update(
                &sc1,
                &SubchannelState {
                    connectivity_state: ConnectivityState::Ready,
                    last_connection_error: None,
                },
            );
        }

        fn simulate_failed_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) {
            self.simulate_connection(addrs, attrs);

            // Simulating TransientFailure for addr1.
            let sc1 = self.policy.subchannels[0].clone();
            self.send_subchannel_update(
                &sc1,
                &SubchannelState {
                    connectivity_state: ConnectivityState::TransientFailure,
                    last_connection_error: Some("connection refused".to_string()),
                },
            );
        }
    }

    // The LB can successfully connect to the first address, and updates the
    // picker to READY with the correct subchannel.
    #[tokio::test]
    async fn test_pick_first_basic_connection() {
        let addrs = vec!["addr1", "addr2"];
        let mut env = new_env();
        env.simulate_successful_connection(addrs, None);

        // Should update picker to READY with sc1.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let res = state.picker.pick(PickOptions::new(
            &RequestHeaders::default(),
            &CallAttributes::new(),
        ));
        match res {
            PickResult::Pick(pick) => {
                assert_eq!(pick.subchannel.address().address.to_string(), "addr1");
            }
            other => panic!("unexpected pick result {:?}", other),
        }
    }

    // If the first address fails, the LB should failover to the second address.
    #[tokio::test]
    async fn test_pick_first_failover() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1", "addr2"], None);

        // Should connect to addr2.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");

        // Simulate addr2 succeeding.
        let sc2 = env.policy.subchannels[1].clone();
        env.send_subchannel_update(
            &sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
        );

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
    }

    // Ensures that if a subchannel is already selected, and is still present in
    // the new resolver update, the LB will keep using it and not switch to a
    // different subchannel.
    #[tokio::test]
    async fn test_pick_first_stickiness() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        // Expect `UpdatePicker(Ready)`.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);

        // New resolver update including addr1.
        let endpoints_new = create_endpoints(vec!["addr2", "addr1", "addr3"], None);
        env.send_resolver_update(endpoints_new).unwrap();

        // Should create new subchannel for addr2 (was cleared by cleanup).
        let sc2 = env.expect_new_subchannel();
        assert_eq!(sc2.address().address.to_string(), "addr2");
        // Should create new subchannel for addr3 (was not in previous list).
        let sc3 = env.expect_new_subchannel();
        assert_eq!(sc3.address().address.to_string(), "addr3");

        // Should NOT have any more events (no Connect, no UpdatePicker),
        // because it stuck to the original selected subchannel.
        env.expect_no_events();

        assert_eq!(
            env.policy
                .selected
                .as_ref()
                .unwrap()
                .address()
                .address
                .to_string(),
            "addr1"
        );
    }

    // If all addresses fail during a connection pass, the LB should update to
    // TransientFailure and request re-resolution.
    #[tokio::test]
    async fn test_pick_first_exhaustion() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1"], None);

        // Should update picker to TransientFailure.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );

        // Should request re-resolution.
        env.expect_request_resolution();
    }

    // Shuffling and interleaving of addresses is deterministic and correct
    // based on the provided shuffler and config.
    #[tokio::test]
    async fn test_pick_first_shuffling_and_interleaving_deterministic() {
        let mut env = new_env();

        // Enable shuffling in config.
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
                        network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                        ..Default::default()
                    },
                    Address {
                        address: crate::byte_str::ByteStr::from("127.0.0.1".to_string()),
                        network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from("::2".to_string()),
                    network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from("127.0.0.2".to_string()),
                    network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];

        env.policy
            .resolver_update(
                ResolverUpdate {
                    endpoints: Ok(endpoints),
                    ..Default::default()
                },
                &config,
                &mut env.tcc,
            )
            .unwrap();

        const NUM_ADDRS: usize = 4;
        let mut resulting_addrs = Vec::with_capacity(NUM_ADDRS);
        for _ in 0..NUM_ADDRS {
            let sc = env.expect_new_subchannel();
            resulting_addrs.push(sc.address().address.to_string());
        }

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

    // De-duplicate addresses that appear multiple times within the same
    // endpoint, and across different endpoints. One subchannel each.
    #[tokio::test]
    async fn test_pick_first_duplicate_de_duplication() {
        let mut env = new_env();

        // Create endpoints with duplicates.
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

        env.send_resolver_update(endpoints).unwrap();

        // Should only create subchannels for addr1 and addr2 (2 unique addrs).
        let sc1 = env.expect_new_subchannel();
        assert_eq!(sc1.address().address.to_string(), "addr1");
        let sc2 = env.expect_new_subchannel();
        assert_eq!(sc2.address().address.to_string(), "addr2");

        // Verify no 3rd subchannel was created.
        while let Ok(event) = env.rx_events.try_recv() {
            if let TestEvent::NewSubchannel(_) = event {
                panic!("Duplicate subchannel created");
            }
        }

        assert_eq!(env.policy.subchannels.len(), 2, "De-duplication failed");
    }

    // If the resolver update contains no addresses, the LB should clear
    // subchannels, update to TransientFailure, and request re-resolution.
    #[tokio::test]
    async fn test_pick_first_empty_update_clears_state() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        // Verify that the policy produced a picker that was READY.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);

        while env.rx_events.try_recv().is_ok() {}

        // Send empty update.
        let res = env.send_resolver_update(vec![]);

        assert!(res.is_err());

        // Check picker is in TransientFailure.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );

        // Check that re-resolution was requested.
        env.expect_request_resolution();
    }

    // If the timer expires during a connection pass, the LB should advance to
    // the next subchannel and trigger a connection attempt.
    #[tokio::test]
    async fn test_pick_first_timer_advancement() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1", "addr2"], None);

        // Simulate timer expiration by setting the flag directly.
        env.policy
            .timer
            .as_ref()
            .unwrap()
            .expired
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Manually call work() to process the timer expiration.
        env.policy.work(None, &mut env.tcc);

        // Expect Connect event for addr2 due to timer expiration.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");
    }

    // If all addresses fail during a connection pass, the LB should enter
    // steady state and monitor for backoff expiry to retry connections.
    #[tokio::test]
    async fn test_pick_first_steady_state_retries() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1"], None);
        let sc1 = env.policy.subchannels[0].clone();

        // Expect UpdatePicker(TransientFailure) and RequestResolution.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();

        // Ensure steady state was entered.
        assert!(env.policy.steady_state.is_some());

        // Simulate addr1 transitioning to IDLE (backoff over).
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
        );

        // Should automatically call connect() again.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");
    }

    // If the LB is in steady state, and a new address becomes ready, it should
    // switch to it immediately. If the current active address goes idle, it
    // should trigger a retry, but should not switch back to it until it reports
    // ready.
    #[tokio::test]
    async fn test_pick_first_steady_state_multi_backend() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        let sc1 = env.policy.subchannels[0].clone();

        // Should failover to addr2: expect Connect(addr2).
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");

        // While addr2 is connecting, simulate addr1 going IDLE (backoff over).
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
        );

        // We should NOT reconnect to addr1 during the first pass.
        // Wait a bit to ensure no event is sent.
        env.expect_no_events();

        // Now fail addr2 to complete first pass.
        let sc2 = env.policy.subchannels[1].clone();
        env.send_subchannel_update(
            &sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("connection refused".to_string()),
            },
        );

        // Expect UpdatePicker(TransientFailure), RequestResolution, and Connect(addr1) from first pass exhaustion.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");

        // Confirm LB is in steady state.
        assert!(env.policy.steady_state.is_some());

        // Simulate addr1 going IDLE again.
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
        );

        // Now it should automatically call connect() again.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");

        // Simulate addr1 successfully connecting and becoming READY.
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
        );

        // The policy should switch to it immediately (enter READY state).
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let res = state.picker.pick(PickOptions::new(
            &RequestHeaders::default(),
            &CallAttributes::new(),
        ));
        match res {
            PickResult::Pick(pick) => {
                assert_eq!(pick.subchannel.address().address.to_string(), "addr1");
            }
            other => panic!("unexpected pick result {:?}", other),
        }
    }

    // If the LB is in steady state, and all addresses fail, it should trigger a
    // re-resolution. If one of the addresses goes idle during this time, it
    // should trigger an immediate connection attempt, rather than waiting for
    // the timer. This prevents the load balancer from getting stuck in idle if
    // all addresses fail at the same time.
    #[tokio::test]
    async fn test_pick_first_steady_state_stuck_idle_prevention() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        let sc1 = env.policy.subchannels[0].clone();

        // Expect Connect(addr2).
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");

        // Simulate addr1 backing off and transitioning to IDLE early
        // (while addr2 is still connecting).
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
        );

        // Expect NO events yet because first pass is still active.
        env.expect_no_events();

        // Fail addr2 to exhaust the first pass.
        let sc2 = env.policy.subchannels[1].clone();
        env.send_subchannel_update(
            &sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("connection refused".to_string()),
            },
        );

        // Expect UpdatePicker(TransientFailure) and RequestResolution from
        // exhaustion.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();

        // Expect an immediate Connect(addr1) event triggered by the exhaustion
        // loop sweeping up the early IDLE subchannel.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");
    }

    // This test is meant to validate that if a new address with different
    // attributes is sent as part of a resolver update, the policy treats it as
    // a different address and creates a new subchannel for it, rather than
    // ignoring it as a duplicate.
    #[tokio::test]
    async fn test_pick_first_address_update_with_attributes() {
        let addr = "addr1";
        let mut env = new_env();
        env.simulate_connection(vec![addr], None);

        // Push same address but with attributes.
        let attrs = crate::attributes::Attributes::new().add("metadata_value".to_string());
        let endpoints_updated = create_endpoints(vec![addr], Some(attrs));

        env.send_resolver_update(endpoints_updated).unwrap();

        // This should be a different subchannel due to different attributes.
        // Therefore, expect a new TestEvent::NewSubchannel event to be emitted.
        let mut found_new_subchannel = false;
        while let Ok(event) = env.rx_events.try_recv() {
            if let TestEvent::NewSubchannel(_) = event {
                found_new_subchannel = true;
                break;
            }
        }

        assert!(
            found_new_subchannel,
            "Policy failed to recreate subchannel when address attributes mutated."
        );
    }

    // If a resolver error is received while the LB is in the process of
    // connecting to addresses, it should not abort the connection attempt or
    // clear the existing addresses, as long as there are still valid addresses
    // in the LB. This ensures that transient resolver errors do not cause
    // unnecessary disruption to active connection attempts.
    #[tokio::test]
    async fn test_pick_first_resolver_error_during_connecting() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1"], None);

        // Simulate resolver error arriving.
        let resolver_error = "dns resolution failed".to_string();
        env.send_resolver_error(resolver_error).unwrap();

        // Resolver errors received during active connection attempts should NOT
        // abort the attempt or force TransientFailure immediately if the load
        // balancer still has valid addresses.
        // Expect NO events to be emitted (no UpdatePicker/RequestResolution).
        env.expect_no_events();

        // Verify internal state did not clear endpoints.
        assert!(
            !env.policy.subchannels.is_empty(),
            "Subchannels erroneously cleared by resolver error."
        );
    }

    // Out-of-Order Failure Detection
    // Ensures the policy waits for all parallel connection attempts to drop
    // before failing the channel.
    #[tokio::test]
    async fn test_pick_first_happy_eyeballs_out_of_order_failure() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1", "addr2"], None);

        // 1. Simulate Happy Eyeballs timer firing to launch parallel connection
        // to addr2.
        env.policy
            .timer
            .as_ref()
            .unwrap()
            .expired
            .store(true, Ordering::SeqCst);
        env.policy.work(None, &mut env.tcc);

        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");

        // 2. Simulate addr2 failing first while addr1 is still in flight.
        let sc2 = env.policy.subchannels[1].clone();
        env.send_subchannel_update(
            &sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("addr2 failed".to_string()),
            },
        );

        // Verify policy does NOT enter TransientFailure yet.
        env.expect_no_events();

        // 3. Simulate addr1 failing. Pass is now fully exhausted.
        let sc1 = env.policy.subchannels[0].clone();
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("addr1 failed".to_string()),
            },
        );

        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
    }

    // Freshest Error Caching (Steady State)
    // Ensures background failures during Steady State continuously overwrite
    // stale connection errors.
    #[tokio::test]
    async fn test_pick_first_steady_state_freshest_error() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1"], None);

        // Consume exhaustion events.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();
        assert!(env.policy.steady_state.is_some());

        // Simulate background failure during Steady State with net-new error telemetry.
        let sc1 = env.policy.subchannels[0].clone();
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("steady state network drop".to_string()),
            },
        );

        // Verify policy caches the freshest unselected error.
        assert_eq!(
            env.policy.last_connection_error.as_deref(),
            Some("steady state network drop")
        );
    }

    // Tests that when a selected subchannel disconnects (transitions to Idle),
    // the policy reports an Idle state and uses an IdlePicker.
    // When an RPC occurs, the IdlePicker schedules work, and the policy
    // reconnects when the work scheduler runs.
    #[tokio::test]
    async fn test_pick_first_disconnect_to_idle_and_reconnect() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1"], None);

        // 1. Consume the initial Ready picker update.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let res = state.picker.pick(PickOptions::new(
            &RequestHeaders::default(),
            &CallAttributes::new(),
        ));
        let sc1 = match res {
            PickResult::Pick(pick) => {
                assert_eq!(pick.subchannel.address().address.to_string(), "addr1");
                pick.subchannel
            }
            other => panic!("unexpected pick result {:?}", other),
        };

        // 2. Simulate the subchannel disconnecting (transitioning to Idle).
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
        );

        // 3. Verify the policy updates the picker to Idle state.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Idle);
        let idle_picker = state.picker;

        // At this point, there should be no more events, as we are waiting for an RPC.
        env.expect_no_events();

        // 4. Simulate an RPC (pick) happening.
        let pick_result = idle_picker.pick(PickOptions::new(
            &RequestHeaders::default(),
            &CallAttributes::new(),
        ));
        assert!(matches!(pick_result, PickResult::Queue));

        // 5. The picker should schedule work.
        let data = env.expect_schedule_work();

        // 6. Call work to execute the scheduled connection attempt.
        env.policy.work(data, &mut env.tcc);

        // 7. Verify that the policy initiates a reconnection to addr1.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");

        // And the picker goes to Connecting.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
    }
}
