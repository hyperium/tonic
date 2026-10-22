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

use std::collections::HashSet;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

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
use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
use crate::core::Address;
use crate::metadata::MetadataMap;
use crate::rt::BoxedTaskHandle;
use crate::rt::GrpcRuntime;

pub static POLICY_NAME: &str = "pick_first";

// The Connection Attempt Delay (gRFC A61): how long to wait for a connection
// attempt before also starting one on the next address.
const CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_millis(250);

type ShufflerFn = dyn Fn(&mut [Endpoint]) + Send + Sync + 'static;

#[derive(Debug, serde::Deserialize, Default)]
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
            shuffler: build_shuffler(),
            state: PickFirstState::Idle(IdleState {
                addresses: Vec::new(),
            }),
            // Not yet reported. IDLE, not CONNECTING, so that `update_picker`
            // doesn't drop the first CONNECTING as a repeat.
            connectivity_state: ConnectivityState::Idle,
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

// A load balancing policy that receives endpoints from the name resolver and
// connects to the first available backend using the Happy Eyeballs connection
// algorithm (https://datatracker.ietf.org/doc/html/rfc8305).
pub(crate) struct PickFirstPolicy {
    work_scheduler: Arc<dyn WorkScheduler>,
    runtime: GrpcRuntime,
    // Injectable shuffler for deterministic testing.
    shuffler: Arc<ShufflerFn>,
    state: PickFirstState, // Current state of the LB's finite state machine.
    // The connectivity state most recently reported to the channel. This is
    // distinct from the LB's internal `state`: a first pass started while
    // in TRANSIENT_FAILURE keeps reporting TRANSIENT_FAILURE (sticky-TF, gRFC
    // A62), even when the LB's internal state is FirstPass.
    connectivity_state: ConnectivityState,
}

impl Debug for PickFirstPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickFirstPolicy")
            .field("state", &self.state)
            .field("connectivity_state", &self.connectivity_state)
            .finish_non_exhaustive()
    }
}

// An address family, for interleaving addresses as per RFC 8305 section 4
// (gRFC A61).
#[derive(PartialEq)]
enum AddressFamily {
    Ipv4,
    Ipv6,
    // Not an IP address and port, e.g. a hostname or a Unix socket path.
    // Neither gRFC A61 nor RFC 8305 specifies how to handle non-IP addresses;
    // grouping them into a third family that participates in interleaving is
    // intentional and matches Go.
    Unknown,
}

impl From<&Address> for AddressFamily {
    fn from(addr: &Address) -> Self {
        if addr.network_type != TCP_IP_NETWORK_TYPE {
            return Self::Unknown;
        }
        // An IPv4-mapped IPv6 address counts as IPv4, as in Go and Java.
        match addr.address.parse::<SocketAddr>() {
            Ok(sa) if sa.ip().to_canonical().is_ipv4() => Self::Ipv4,
            Ok(_) => Self::Ipv6,
            Err(_) => Self::Unknown,
        }
    }
}

impl PickFirstPolicy {
    // Converts the update endpoints to an address list.
    // Shuffles endpoints (if enabled) before flattening, de-duplication and
    // interleaving (gRFC A61). Returns an error if the list is empty.
    fn compile_address(
        &self,
        mut endpoints: Vec<Endpoint>,
        config: &PickFirstConfig,
    ) -> Result<Vec<Address>, String> {
        // Shuffle endpoints if enabled.
        if config.shuffle_address_list {
            (self.shuffler)(&mut endpoints);
        }

        // Flatten and de-duplicate unique addresses in order, grouping them by
        // address family. Families are kept in order of first appearance.
        let mut seen = HashSet::new();
        let mut families: Vec<(AddressFamily, Vec<Address>)> = Vec::new();
        for addr in endpoints.into_iter().flat_map(|ep| ep.addresses) {
            if !seen.insert(addr.clone()) {
                continue;
            }
            let family = AddressFamily::from(&addr);
            match families.iter_mut().find(|(f, _)| *f == family) {
                Some((_, addrs)) => addrs.push(addr),
                None => families.push((family, vec![addr])),
            }
        }

        // Interleave the families as per RFC 8305 section 4: take one address
        // from each family in turn, starting with the family of the first
        // address.
        let mut families: Vec<_> = families
            .into_iter()
            .map(|(_, addrs)| addrs.into_iter())
            .collect();
        let mut interleaved = Vec::with_capacity(seen.len());
        while interleaved.len() < seen.len() {
            for addrs in &mut families {
                if let Some(addr) = addrs.next() {
                    interleaved.push(addr);
                }
            }
        }

        if interleaved.is_empty() {
            return Err("empty address list".to_string());
        }

        Ok(interleaved)
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        controller: &mut dyn ChannelController,
    ) {
        let mut ctx = PickFirstContext {
            runtime: &self.runtime,
            work_scheduler: &self.work_scheduler,
            connectivity_state: &mut self.connectivity_state,
            controller,
        };
        let current = std::mem::take(&mut self.state);
        self.state = current.subchannel_update(&mut ctx, subchannel, state);
    }

    // Handles a resolver update whose endpoints are in an error state.
    fn resolver_error(
        &mut self,
        error: String,
        controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let mut ctx = PickFirstContext {
            runtime: &self.runtime,
            work_scheduler: &self.work_scheduler,
            connectivity_state: &mut self.connectivity_state,
            controller,
        };

        // No previous good update: there is nothing to connect to.
        if matches!(&self.state, PickFirstState::Idle(s) if s.addresses.is_empty()) {
            self.state = SteadyState::enter(&mut ctx, Vec::new(), error.clone());
            return Err(error);
        }

        // Keep using the previous addresses. If already failing, surface the
        // resolver error in the picker.
        if ctx.connectivity_state() == ConnectivityState::TransientFailure {
            ctx.set_failing_picker(&error);
        }
        // Returning the error tells the resolver to re-resolve with backoff.
        Err(error)
    }
}

impl LbPolicy for PickFirstPolicy {
    type LbConfig = PickFirstConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let addresses = match update.endpoints {
            Ok(endpoints) => self.compile_address(endpoints, config),
            Err(error) => return self.resolver_error(error, controller),
        };

        let mut ctx = PickFirstContext {
            runtime: &self.runtime,
            work_scheduler: &self.work_scheduler,
            connectivity_state: &mut self.connectivity_state,
            controller,
        };

        match addresses {
            Ok(addresses) => {
                let current = std::mem::take(&mut self.state);
                self.state = current.resolver_update(&mut ctx, addresses);
                Ok(())
            }
            Err(e) => {
                self.state = SteadyState::enter(&mut ctx, Vec::new(), e.clone());
                Err(e)
            }
        }
    }

    fn work(&mut self, data: Option<WorkData>, controller: &mut dyn ChannelController) {
        let work = match PickFirstWork::try_from(data) {
            Ok(work) => work,
            Err(data) => {
                debug_assert!(
                    false,
                    "work called with {data:?}; expected SubchannelUpdate or PickFirstWork"
                );
                return;
            }
        };
        match work {
            PickFirstWork::Subchannel(update) => {
                self.subchannel_update(update.subchannel, &update.state, controller);
            }
            PickFirstWork::LeaveIdle => {
                self.exit_idle(controller);
            }
            PickFirstWork::TimerExpired => {
                let mut ctx = PickFirstContext {
                    runtime: &self.runtime,
                    work_scheduler: &self.work_scheduler,
                    connectivity_state: &mut self.connectivity_state,
                    controller,
                };
                let current = std::mem::take(&mut self.state);
                self.state = current.timer_expired(&mut ctx);
            }
        }
    }

    fn exit_idle(&mut self, controller: &mut dyn ChannelController) {
        let mut ctx = PickFirstContext {
            runtime: &self.runtime,
            work_scheduler: &self.work_scheduler,
            connectivity_state: &mut self.connectivity_state,
            controller,
        };
        let current = std::mem::take(&mut self.state);
        self.state = current.exit_idle(&mut ctx);
    }
}

struct PickFirstContext<'a> {
    runtime: &'a GrpcRuntime,
    work_scheduler: &'a Arc<dyn WorkScheduler>,
    // The policy's reported connectivity state. Only `update_picker` writes
    // it.
    connectivity_state: &'a mut ConnectivityState,
    // Publish pickers via `update_picker` rather than directly, so that
    // `connectivity_state` stays accurate.
    controller: &'a mut dyn ChannelController,
}

impl PickFirstContext<'_> {
    // Returns the connectivity state most recently reported to the channel.
    fn connectivity_state(&self) -> ConnectivityState {
        *self.connectivity_state
    }

    // Reports `connectivity_state` and `picker` to the channel. A repeat of
    // IDLE or CONNECTING is dropped, since its picker behaves like the current
    // one.
    fn update_picker(&mut self, connectivity_state: ConnectivityState, picker: Arc<dyn Picker>) {
        if connectivity_state == *self.connectivity_state
            && matches!(
                connectivity_state,
                ConnectivityState::Idle | ConnectivityState::Connecting
            )
        {
            return;
        }
        *self.connectivity_state = connectivity_state;
        self.controller.update_picker(LbState {
            connectivity_state,
            picker,
        });
    }

    fn set_failing_picker(&mut self, error: &str) {
        self.update_picker(
            ConnectivityState::TransientFailure,
            Arc::new(FailingPicker {
                error: error.to_string(),
            }),
        );
    }
}

// State node: Idle.
#[derive(Debug, Default)]
struct IdleState {
    addresses: Vec<Address>,
}

impl IdleState {
    fn enter(ctx: &mut PickFirstContext<'_>, addresses: Vec<Address>) -> PickFirstState {
        let picker = Arc::new(IdlePicker::new(ctx.work_scheduler.clone()));
        ctx.update_picker(ConnectivityState::Idle, picker);
        PickFirstState::Idle(IdleState { addresses })
    }

    fn resolver_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
    ) -> PickFirstState {
        // No previous good update: connect as soon as the initial address list
        // arrives (gRFC A61). After a disconnect, stay IDLE until a pick or
        // `exit_idle`.
        if self.addresses.is_empty() {
            return FirstPassState::fresh_enter(ctx, addresses);
        }
        Self::enter(ctx, addresses)
    }

    fn subchannel_update(
        self,
        _ctx: &mut PickFirstContext<'_>,
        _subchannel: Arc<dyn Subchannel>,
        _state: &SubchannelState,
    ) -> PickFirstState {
        PickFirstState::Idle(self)
    }

    fn exit_idle(self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        // No address list yet: nothing to connect to. The first resolver
        // update starts connecting.
        if self.addresses.is_empty() {
            return PickFirstState::Idle(self);
        }
        FirstPassState::fresh_enter(ctx, self.addresses)
    }
}

// An entry associating a created subchannel handle with its cached
// connectivity state.
#[derive(Debug)]
struct SubchannelEntry {
    subchannel: Arc<dyn Subchannel>,
    // The state most recently reported by the subchannel.
    state: SubchannelState,
    // Whether a connection attempt failed during the current first pass.
    // Reset when a first pass starts; only read by `FirstPassState`.
    failed_in_first_pass: bool,
}

// Returns the position of `subchannel` in `entries`, or `None` if it isn't
// there (an outdated subchannel from an earlier list). Matches the subchannel
// object itself, not its address: a subchannel from an earlier list is
// outdated even if its address is still present.
fn index_of(entries: &[SubchannelEntry], subchannel: &Arc<dyn Subchannel>) -> Option<usize> {
    entries
        .iter()
        .position(|e| Arc::ptr_eq(&e.subchannel, subchannel))
}

// Returns the addresses of the subchannels in `entries`, in order.
fn entry_addresses(entries: &[SubchannelEntry]) -> Vec<Address> {
    entries.iter().map(|e| e.subchannel.address()).collect()
}

// State node: FirstPass (Happy Eyeballs).
#[derive(Debug)]
struct FirstPassState {
    subchannels: Vec<SubchannelEntry>,
    frontier_index: usize,
    // The Connection Attempt Delay timer for the current connection attempt.
    // `None` when attempting the last subchannel in the list, or once the
    // pass has moved past the end of the list.
    timer: Option<Timer>,
    last_connection_error: Option<String>,
}

impl FirstPassState {
    fn fresh_enter(ctx: &mut PickFirstContext<'_>, addresses: Vec<Address>) -> PickFirstState {
        Self::enter(ctx, addresses, Vec::new())
    }

    fn enter(
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
        mut existing: Vec<SubchannelEntry>,
    ) -> PickFirstState {
        let mut new_subchannels = Vec::with_capacity(addresses.len());

        for addr in &addresses {
            let entry = if let Some(pos) = existing
                .iter()
                .position(|e| e.subchannel.address() == *addr)
            {
                // A new first pass starts with no failures recorded.
                let mut entry = existing.swap_remove(pos);
                entry.failed_in_first_pass = false;
                entry
            } else {
                // Updates for the new subchannel are delivered to our work
                // method via our work scheduler.
                let (subchannel, state) = ctx
                    .controller
                    .new_subchannel(addr, ctx.work_scheduler.clone());
                SubchannelEntry {
                    subchannel,
                    state,
                    failed_in_first_pass: false,
                }
            };

            if entry.state.connectivity_state == ConnectivityState::Ready {
                return ReadyState::enter(ctx, addresses, entry.subchannel);
            }

            new_subchannels.push(entry);
        }

        // Subchannels already in TRANSIENT_FAILURE count as failed in this
        // pass. Start from the last of their errors in list order, so that the
        // pass reports a real connection error even if no subchannel fails
        // again.
        let last_connection_error = new_subchannels
            .iter()
            .rev()
            .filter(|e| e.state.connectivity_state == ConnectivityState::TransientFailure)
            .find_map(|e| e.state.last_connection_error.clone());

        let mut first_pass = FirstPassState {
            subchannels: new_subchannels,
            frontier_index: 0,
            timer: None,
            last_connection_error,
        };

        if first_pass.advance_frontier(true) {
            first_pass.start_connection_attempt(ctx);

            // No subchannel is READY here (checked above). Sticky
            // TRANSIENT_FAILURE (gRFC A62): if already reporting
            // TRANSIENT_FAILURE, don't switch to CONNECTING; keep the
            // failing picker. ReadyState::enter reports READY once a
            // subchannel connects.
            if ctx.connectivity_state() != ConnectivityState::TransientFailure {
                ctx.update_picker(ConnectivityState::Connecting, Arc::new(QueuingPicker {}));
            }

            PickFirstState::FirstPass(first_pass)
        } else {
            first_pass.enter_steady_state(ctx)
        }
    }

    // Ensures error is carried into the steady state transition.
    fn enter_steady_state(self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        let err = self
            .last_connection_error
            .unwrap_or_else(|| "all addresses in transient failure".to_string());
        SteadyState::enter(ctx, self.subchannels, err)
    }

    // Starts a connection attempt on the subchannel the pass has reached. Only
    // requests a connection if the subchannel last reported IDLE; if it is
    // already CONNECTING, waits for that attempt.
    fn start_connection_attempt(&mut self, ctx: &mut PickFirstContext<'_>) {
        let entry = &self.subchannels[self.frontier_index];
        if entry.state.connectivity_state == ConnectivityState::Idle {
            entry.subchannel.connect();
        }
        // Start the Connection Attempt Delay timer unless this is the last
        // subchannel in the list (gRFC A61). Replacing the timer cancels any
        // previous one.
        let is_last = self.frontier_index + 1 >= self.subchannels.len();
        self.timer =
            (!is_last).then(|| Timer::start(ctx.runtime.clone(), ctx.work_scheduler.clone()));
    }

    // Moves the pass to the next subchannel not in TRANSIENT_FAILURE,
    // starting from the beginning if `reset`. Subchannels skipped because they
    // are in TRANSIENT_FAILURE count as failed in this pass. Returns false if
    // the pass moved past the end of the list.
    fn advance_frontier(&mut self, reset: bool) -> bool {
        if reset {
            self.frontier_index = 0;
        } else {
            self.frontier_index += 1;
        }

        while let Some(entry) = self.subchannels.get_mut(self.frontier_index) {
            if entry.state.connectivity_state != ConnectivityState::TransientFailure {
                return true;
            }
            entry.failed_in_first_pass = true;
            self.frontier_index += 1;
        }
        false
    }

    fn resolver_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
    ) -> PickFirstState {
        FirstPassState::enter(ctx, addresses, self.subchannels)
    }

    fn subchannel_update(
        mut self,
        ctx: &mut PickFirstContext<'_>,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) -> PickFirstState {
        let Some(index) = index_of(&self.subchannels, &subchannel) else {
            // This update is from an outdated subchannel that is no longer in
            // the subchannel list. Ignore it.
            return PickFirstState::FirstPass(self);
        };

        let entry = &mut self.subchannels[index];
        entry.state = state.clone();

        if state.connectivity_state == ConnectivityState::Ready {
            let addresses = entry_addresses(&self.subchannels);
            return ReadyState::enter(ctx, addresses, subchannel);
        }

        if state.connectivity_state == ConnectivityState::TransientFailure {
            if let Some(err) = &state.last_connection_error {
                self.last_connection_error = Some(err.clone());
            }

            // Only count failures of subchannels this pass has reached. A
            // subchannel the pass has not reached yet may be failing an attempt
            // this pass did not start, and may be IDLE again by the time the
            // pass reaches it.
            if index <= self.frontier_index {
                entry.failed_in_first_pass = true;
            }

            // The current connection attempt failed: cancel its timer and move
            // on to the next subchannel, if any.
            if index == self.frontier_index {
                self.timer = None;
                if self.advance_frontier(false) {
                    self.start_connection_attempt(ctx);
                }
            }
        }

        if self.is_complete() {
            return self.enter_steady_state(ctx);
        }

        PickFirstState::FirstPass(self)
    }

    // Returns whether the first pass is complete: every subchannel has been
    // reached and has failed at least once during this pass (gRFC A61).
    fn is_complete(&self) -> bool {
        self.frontier_index >= self.subchannels.len()
            && self.subchannels.iter().all(|e| e.failed_in_first_pass)
    }

    // Handles a TimerExpired wakeup. The wakeup may come from the timer of an
    // earlier first pass; only act if the current timer has expired.
    fn timer_expired(mut self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        if self.timer.take_if(|t| t.expired()).is_some() && self.advance_frontier(false) {
            self.start_connection_attempt(ctx);
        }
        PickFirstState::FirstPass(self)
    }
}

// State node: SteadyState (Sticky TRANSIENT_FAILURE).
#[derive(Debug)]
struct SteadyState {
    subchannels: Vec<SubchannelEntry>,
    failure_count: usize,
    last_connection_error: String,
}

impl SteadyState {
    fn enter(
        ctx: &mut PickFirstContext<'_>,
        subchannels: Vec<SubchannelEntry>,
        last_error: String,
    ) -> PickFirstState {
        ctx.set_failing_picker(&last_error);
        ctx.controller.request_resolution();

        for entry in &subchannels {
            if entry.state.connectivity_state == ConnectivityState::Idle {
                entry.subchannel.connect();
            }
        }

        PickFirstState::SteadyState(SteadyState {
            failure_count: 0,
            last_connection_error: last_error,
            subchannels,
        })
    }

    fn resolver_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
    ) -> PickFirstState {
        FirstPassState::enter(ctx, addresses, self.subchannels)
    }

    fn subchannel_update(
        mut self,
        ctx: &mut PickFirstContext<'_>,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) -> PickFirstState {
        let Some(index) = index_of(&self.subchannels, &subchannel) else {
            // This update is from an outdated subchannel that is no longer in
            // the subchannel list. Ignore it.
            return PickFirstState::SteadyState(self);
        };
        self.subchannels[index].state = state.clone();

        match state.connectivity_state {
            ConnectivityState::Ready => {
                let addresses = entry_addresses(&self.subchannels);
                ReadyState::enter(ctx, addresses, subchannel)
            }
            ConnectivityState::Idle => {
                subchannel.connect();
                PickFirstState::SteadyState(self)
            }
            ConnectivityState::TransientFailure => {
                if let Some(err) = &state.last_connection_error {
                    self.last_connection_error.clone_from(err);
                }
                // Every N failures, where N is the number of subchannels,
                // report the latest error and request re-resolution
                // (gRFC A61).
                self.failure_count += 1;
                if self.failure_count >= self.subchannels.len() {
                    self.failure_count = 0;
                    ctx.set_failing_picker(&self.last_connection_error);
                    ctx.controller.request_resolution();
                }
                PickFirstState::SteadyState(self)
            }
            ConnectivityState::Connecting => PickFirstState::SteadyState(self),
        }
    }
}

// State node: Ready.
#[derive(Debug)]
struct ReadyState {
    addresses: Vec<Address>,
    selected: Arc<dyn Subchannel>,
}

impl ReadyState {
    fn enter(
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
        selected: Arc<dyn Subchannel>,
    ) -> PickFirstState {
        let picker = Arc::new(OneSubchannelPicker {
            sc: selected.clone(),
        });
        ctx.update_picker(ConnectivityState::Ready, picker);
        PickFirstState::Ready(ReadyState {
            addresses,
            selected,
        })
    }

    fn resolver_update(
        mut self,
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
    ) -> PickFirstState {
        let selected_addr = self.selected.address();
        if addresses.contains(&selected_addr) {
            self.addresses = addresses;
            return PickFirstState::Ready(self);
        }

        FirstPassState::fresh_enter(ctx, addresses)
    }

    fn subchannel_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) -> PickFirstState {
        if Arc::ptr_eq(&self.selected, &subchannel)
            && state.connectivity_state != ConnectivityState::Ready
        {
            // The connection was lost, so the addresses may be stale.
            // Request re-resolution.
            ctx.controller.request_resolution();
            IdleState::enter(ctx, self.addresses)
        } else {
            PickFirstState::Ready(self)
        }
    }
}

// `PickFirstPolicy` is structured as a discrete finite state machine with
// these states:
// - `Idle`: Initial state until the first address list arrives, which starts
//   a connection attempt. After a disconnect, waits for a pick or `exit_idle`
//   to trigger a connection attempt.
// - `FirstPass`: Happy Eyeballs pass (gRFC A61) staggering connection attempts
//   across resolved addresses.
// - `SteadyState`: All addresses failed, or there are none; holds sticky
//   TRANSIENT_FAILURE and retries connections as backoffs expire.
// - `Ready`: Successfully connected to a subchannel, routing all picks to it.
//
// Each state publishes pickers through `PickFirstContext::update_picker`,
// which records the reported connectivity state.
#[derive(Debug)]
enum PickFirstState {
    Idle(IdleState),
    FirstPass(FirstPassState),
    SteadyState(SteadyState),
    Ready(ReadyState),
}

impl Default for PickFirstState {
    fn default() -> Self {
        Self::Idle(IdleState::default())
    }
}

impl PickFirstState {
    fn resolver_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
    ) -> PickFirstState {
        match self {
            Self::Idle(s) => s.resolver_update(ctx, addresses),
            Self::FirstPass(s) => s.resolver_update(ctx, addresses),
            Self::SteadyState(s) => s.resolver_update(ctx, addresses),
            Self::Ready(s) => s.resolver_update(ctx, addresses),
        }
    }

    fn subchannel_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) -> PickFirstState {
        match self {
            Self::Idle(s) => s.subchannel_update(ctx, subchannel, state),
            Self::FirstPass(s) => s.subchannel_update(ctx, subchannel, state),
            Self::SteadyState(s) => s.subchannel_update(ctx, subchannel, state),
            Self::Ready(s) => s.subchannel_update(ctx, subchannel, state),
        }
    }

    fn timer_expired(self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        match self {
            Self::FirstPass(s) => s.timer_expired(ctx),
            _ => self,
        }
    }

    fn exit_idle(self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        match self {
            Self::Idle(s) => s.exit_idle(ctx),
            _ => self,
        }
    }
}

#[derive(Debug)]
enum PickFirstWork {
    Subchannel(SubchannelUpdate),
    TimerExpired,
    LeaveIdle,
}

impl From<PickFirstWork> for Option<WorkData> {
    fn from(work: PickFirstWork) -> Self {
        Some(Box::new(work))
    }
}

impl TryFrom<Option<WorkData>> for PickFirstWork {
    type Error = Option<WorkData>;

    fn try_from(data: Option<WorkData>) -> Result<Self, Self::Error> {
        let data = data.ok_or(None)?;
        let data = match data.downcast::<SubchannelUpdate>() {
            Ok(update) => return Ok(Self::Subchannel(*update)),
            Err(data) => data,
        };
        data.downcast::<Self>().map(|w| *w).map_err(Some)
    }
}

// The Connection Attempt Delay timer (gRFC A61). `expired` becomes set when
// it fires. When dropped, the timer is cancelled.
struct Timer {
    expired: Arc<AtomicBool>,
    handle: BoxedTaskHandle,
}

impl Timer {
    // Starts a new timer, returning it.
    fn start(runtime: GrpcRuntime, work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        let expired = Arc::new(AtomicBool::new(false));
        let expired_clone = expired.clone();
        let handle = runtime.clone().spawn(Box::pin(async move {
            runtime.sleep(CONNECTION_ATTEMPT_DELAY).await;
            expired_clone.store(true, Ordering::SeqCst);
            work_scheduler.schedule_work(PickFirstWork::TimerExpired.into());
        }));
        Self { expired, handle }
    }

    // Returns whether the timer has expired yet.
    fn expired(&self) -> bool {
        self.expired.load(Ordering::SeqCst)
    }
}

impl Debug for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timer")
            .field("expired", &self.expired())
            .finish()
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
            self.work_scheduler
                .schedule_work(PickFirstWork::LeaveIdle.into());
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

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use crate::client::CallOptions;
    use crate::client::load_balancing::test_utils;
    use crate::client::load_balancing::test_utils::TestChannelController;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::rt::default_runtime;

    // A ChannelController wrapper that delegates to TestChannelController,
    // overriding the initial SubchannelState returned by new_subchannel for
    // configured addresses.
    struct InitialStateController<'a> {
        tcc: &'a mut TestChannelController,
        initial_states: HashMap<&'a str, SubchannelState>,
    }

    impl ChannelController for InitialStateController<'_> {
        fn new_subchannel(
            &mut self,
            address: &Address,
            work_scheduler: Arc<dyn WorkScheduler>,
        ) -> (Arc<dyn Subchannel>, SubchannelState) {
            let (sc, state) = self.tcc.new_subchannel(address, work_scheduler);
            let state = self
                .initial_states
                .get(&*address.address)
                .cloned()
                .unwrap_or(state);
            (sc, state)
        }

        fn update_picker(&mut self, update: LbState) {
            self.tcc.update_picker(update);
        }

        fn request_resolution(&mut self) {
            self.tcc.request_resolution();
        }
    }

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
        // Sends a resolver update for `addrs` while overriding the initial
        // SubchannelState returned by new_subchannel for entries in
        // `initial_states`.
        fn send_resolver_update_with_initial_states(
            &mut self,
            addrs: Vec<&str>,
            initial_states: &[(&str, SubchannelState)],
        ) -> Result<(), String> {
            let update = ResolverUpdate {
                endpoints: Ok(create_endpoints(addrs, None)),
                ..Default::default()
            };
            let mut controller = InitialStateController {
                tcc: &mut self.tcc,
                initial_states: initial_states.iter().cloned().collect(),
            };
            self.policy
                .resolver_update(update, &PickFirstConfig::default(), &mut controller)
        }

        // Helper to simulate a basic connection against a list of
        // addresses. Returns the subchannels created, in address order. Does
        // not imply that the connection succeeded or failed.
        fn simulate_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) -> Vec<Arc<dyn Subchannel>> {
            let endpoints = create_endpoints(addrs.clone(), attrs);
            self.send_resolver_update(endpoints).unwrap();

            let subchannels = self.expect_new_subchannels(&addrs);

            self.expect_connect_to(addrs[0]);
            self.expect_queuing_picker();
            subchannels
        }

        // Simulates a connection where the first address becomes READY, and
        // consumes the resulting READY picker update.
        fn simulate_successful_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) -> Vec<Arc<dyn Subchannel>> {
            let subchannels = self.simulate_connection(addrs, attrs);
            self.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
            self.expect_ready_picker(&subchannels[0]);
            subchannels
        }

        // Simulates a connection where the first address fails with
        // "connection refused". Events resulting from the failure are left for
        // the caller to verify.
        fn simulate_failed_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) -> Vec<Arc<dyn Subchannel>> {
            let subchannels = self.simulate_connection(addrs, attrs);
            self.send_subchannel_update(
                &subchannels[0],
                &SubchannelState::transient_failure("connection refused"),
            );
            subchannels
        }

        // Disconnects the selected subchannel; the policy requests
        // re-resolution and goes IDLE. A pick then makes the policy reconnect
        // with a new subchannel for every address. `addrs` is the address
        // list, starting with the selected subchannel's address. Returns the
        // new subchannels, in address order.
        fn simulate_disconnect_and_reconnect(
            &mut self,
            selected: &Arc<dyn Subchannel>,
            addrs: &[&str],
        ) -> Vec<Arc<dyn Subchannel>> {
            self.send_subchannel_update(selected, &SubchannelState::idle());
            self.expect_request_resolution();
            let picker = self.expect_picker_state(ConnectivityState::Idle);
            self.expect_no_events();
            assert_eq!(
                picker.pick(PickOptions::new(
                    &test_utils::new_request_headers(),
                    &CallOptions::new(),
                )),
                PickResult::Queue
            );
            let data = self.expect_schedule_work();
            assert!(
                matches!(
                    data.as_ref()
                        .and_then(|d| d.downcast_ref::<PickFirstWork>()),
                    Some(PickFirstWork::LeaveIdle)
                ),
                "expected LeaveIdle work, got {data:?}"
            );
            self.policy.work(data, &mut self.tcc);
            let subchannels = self.expect_new_subchannels(addrs);
            self.expect_connect_to(addrs[0]);
            self.expect_queuing_picker();
            subchannels
        }

        // Verifies that the policy created subchannels for `addrs` in order,
        // and returns them.
        fn expect_new_subchannels(&mut self, addrs: &[&str]) -> Vec<Arc<dyn Subchannel>> {
            addrs
                .iter()
                .map(|addr| self.expect_new_subchannel_for(addr))
                .collect()
        }

        // Advances virtual time by `duration` on the paused Tokio runtime,
        // yielding before and after so spawned timer tasks can register on the
        // timer wheel and schedule their completion work.
        async fn advance_time(&mut self, duration: Duration) {
            tokio::task::yield_now().await;
            tokio::time::advance(duration).await;
            tokio::task::yield_now().await;
        }

        // Advances the paused tokio clock past the Connection Attempt Delay,
        // letting any pending timer fire.
        async fn advance_past_timer(&mut self) {
            self.advance_time(CONNECTION_ATTEMPT_DELAY + Duration::from_millis(1))
                .await;
        }

        // Lets the Connection Attempt Delay timer fire and returns the
        // resulting work without delivering it. Requires a paused tokio clock.
        async fn expect_timer_work(&mut self) -> Option<WorkData> {
            self.advance_past_timer().await;
            let data = self.expect_schedule_work();
            assert!(
                matches!(
                    data.as_ref()
                        .and_then(|d| d.downcast_ref::<PickFirstWork>()),
                    Some(PickFirstWork::TimerExpired)
                ),
                "expected TimerExpired work, got {data:?}"
            );
            data
        }

        // Lets the Connection Attempt Delay timer fire and delivers the
        // resulting work to the policy. Requires a paused tokio clock.
        async fn fire_timer(&mut self) {
            let data = self.expect_timer_work().await;
            self.policy.work(data, &mut self.tcc);
        }
    }

    // The LB can successfully connect to the first address, and updates the
    // picker to READY with the correct subchannel (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_basic_connection() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2"], None);

        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker(&scs[0]);
        env.expect_no_events();
    }

    // If the first address fails, the LB should failover to the second address
    // (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_failover() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        env.expect_connect_to("addr2");

        env.send_subchannel_update(&scs[1], &SubchannelState::ready());
        env.expect_ready_picker(&scs[1]);
        env.expect_no_events();
    }

    // Ensures that if a subchannel is already selected, and is still present in
    // the new resolver update, the LB will keep using it and not switch to a
    // different subchannel (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_stickiness() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let endpoints_new = create_endpoints(vec!["addr2", "addr1", "addr3"], None);
        env.send_resolver_update(endpoints_new).unwrap();

        // No new subchannels, connects, or picker updates: the READY picker
        // for addr1 remains in use.
        env.expect_no_events();
    }

    // When an update removes the selected address, the policy starts a new
    // first pass over the new list and reports CONNECTING, as Go does. C++
    // and Java go IDLE instead.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_resolver_update_without_selected_address_starts_first_pass() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let endpoints = create_endpoints(vec!["addr2", "addr3"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_new_subchannels(&["addr2", "addr3"]);
        env.expect_connect_to("addr2");
        env.expect_queuing_picker();
        env.expect_no_events();
    }

    // Shuffling and interleaving of addresses is deterministic and correct
    // based on the provided shuffler and config (gRFC A61, gRFC A62).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_shuffling_and_interleaving_deterministic() {
        let mut env = new_env();

        // Enable shuffling in config.
        let config = PickFirstBuilder {}
            .parse_config(&LbConfigJson::new(r#"{"shuffleAddressList": true}"#).unwrap())
            .unwrap();

        // Provide three endpoints:
        // EP1: [V6_1, V4_1]
        // EP2: [V6_2]
        // EP3: [V4_2]
        let endpoints = vec![
            Endpoint {
                addresses: vec![
                    Address {
                        address: crate::byte_str::ByteStr::from("[::1]:80".to_string()),
                        network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                        ..Default::default()
                    },
                    Address {
                        address: crate::byte_str::ByteStr::from("127.0.0.1:80".to_string()),
                        network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from("[::2]:80".to_string()),
                    network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![Address {
                    address: crate::byte_str::ByteStr::from("127.0.0.2:80".to_string()),
                    network_type: crate::client::name_resolution::TCP_IP_NETWORK_TYPE,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];

        env.send_resolver_update_with_config(endpoints, &config)
            .unwrap();

        // Mock shuffler reverses endpoints: [EP3, EP2, EP1]
        // EP3: [127.0.0.2] (V4)
        // EP2: [::2] (V6)
        // EP1: [::1] (V6), [127.0.0.1] (V4)
        //
        // Categorized:
        // IPv4: [127.0.0.2, 127.0.0.1]
        // IPv6: [::2, ::1]
        //
        // Interleaved, starting with the family of the first address (IPv4):
        // [127.0.0.2, ::2, 127.0.0.1, ::1]
        env.expect_new_subchannels(&["127.0.0.2:80", "[::2]:80", "127.0.0.1:80", "[::1]:80"]);

        env.expect_connect_to("127.0.0.2:80");
        env.expect_queuing_picker();
        env.expect_no_events();
    }

    // Sends one single-address endpoint per entry in `addrs`, as the DNS
    // resolver does, and verifies that the policy creates subchannels and
    // connects in `want` order.
    fn check_address_order(addrs: Vec<&str>, want: &[&str]) {
        let mut env = new_env();
        env.send_resolver_update(create_endpoints(addrs, None))
            .unwrap();
        env.expect_new_subchannels(want);
        env.expect_connect_to(want[0]);
        env.expect_queuing_picker();
        env.expect_no_events();
    }

    // Addresses are interleaved by family as per RFC 8305 section 4 (gRFC
    // A61), starting with the family of the first address, here IPv4. An
    // IPv4-mapped IPv6 address counts as IPv4. Ported from Go's
    // TestPickFirstLeaf_InterleavingIPV4Preferred, with a numeric zone ID in
    // place of %eth0, as Rust's SocketAddr parses only numeric zone IDs.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_interleaving_ipv4_preferred() {
        check_address_order(
            vec![
                "1.1.1.1:1111",
                "2.2.2.2:2",
                "3.3.3.3:3",
                "[::FFFF:192.168.0.1]:2222",
                "[0001:0001:0001:0001:0001:0001:0001:0001]:8080",
                "[0002:0002:0002:0002:0002:0002:0002:0002]:8080",
                "[fe80::1%2]:3333",
                "grpc.io:80",
            ],
            &[
                "1.1.1.1:1111",
                "[0001:0001:0001:0001:0001:0001:0001:0001]:8080",
                "grpc.io:80",
                "2.2.2.2:2",
                "[0002:0002:0002:0002:0002:0002:0002:0002]:8080",
                "3.3.3.3:3",
                "[fe80::1%2]:3333",
                "[::FFFF:192.168.0.1]:2222",
            ],
        );
    }

    // Addresses are interleaved by family, starting with the family of the
    // first address, here IPv6 (gRFC A61). Ported from Go's
    // TestPickFirstLeaf_InterleavingIPv6Preferred, with a numeric zone ID.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_interleaving_ipv6_preferred() {
        check_address_order(
            vec![
                "[0001:0001:0001:0001:0001:0001:0001:0001]:8080",
                // A duplicate, which is dropped.
                "[0001:0001:0001:0001:0001:0001:0001:0001]:8080",
                "1.1.1.1:1111",
                "2.2.2.2:2",
                "3.3.3.3:3",
                "[::FFFF:192.168.0.1]:2222",
                "[0002:0002:0002:0002:0002:0002:0002:0002]:2222",
                "[fe80::1%2]:3333",
                "grpc.io:80",
            ],
            &[
                "[0001:0001:0001:0001:0001:0001:0001:0001]:8080",
                "1.1.1.1:1111",
                "grpc.io:80",
                "[0002:0002:0002:0002:0002:0002:0002:0002]:2222",
                "2.2.2.2:2",
                "[fe80::1%2]:3333",
                "3.3.3.3:3",
                "[::FFFF:192.168.0.1]:2222",
            ],
        );
    }

    // Addresses that are not an IP address and port, such as hostnames, form
    // a third family for interleaving, here the family of the first address
    // (gRFC A61). Ported from Go's
    // TestPickFirstLeaf_InterleavingUnknownPreferred, with a numeric zone ID.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_interleaving_unknown_preferred() {
        check_address_order(
            vec![
                "grpc.io:80",
                "1.1.1.1:1111",
                "2.2.2.2:2",
                "3.3.3.3:3",
                "[::FFFF:192.168.0.1]:2222",
                "[0001:0001:0001:0001:0001:0001:0001:0001]:8080",
                "[0002:0002:0002:0002:0002:0002:0002:0002]:8080",
                "[fe80::1%2]:3333",
                "example.com:80",
            ],
            &[
                "grpc.io:80",
                "1.1.1.1:1111",
                "[0001:0001:0001:0001:0001:0001:0001:0001]:8080",
                "example.com:80",
                "2.2.2.2:2",
                "[0002:0002:0002:0002:0002:0002:0002:0002]:8080",
                "3.3.3.3:3",
                "[fe80::1%2]:3333",
                "[::FFFF:192.168.0.1]:2222",
            ],
        );
    }

    // De-duplicate addresses that appear multiple times within the same
    // endpoint, and across different endpoints. One subchannel each (gRFC A61).
    #[tokio::test(start_paused = true)]
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
        env.expect_new_subchannels(&["addr1", "addr2"]);
        env.expect_connect_to("addr1");
        env.expect_queuing_picker();
        env.expect_no_events();
    }

    // If the resolver update contains no addresses, the LB should clear
    // subchannels, update to TransientFailure, and request re-resolution
    // (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_empty_update_clears_state() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let res = env.send_resolver_update(vec![]);
        assert_eq!(res.unwrap_err(), "empty address list");

        env.expect_failing_picker("empty address list");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // If the timer expires during the first pass, the LB should advance to
    // the next subchannel and trigger a connection attempt. No timer is
    // started for the last subchannel (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_timer_advancement() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1", "addr2"], None);
        env.expect_no_events();

        env.fire_timer().await;
        env.expect_connect_to("addr2");
        env.expect_no_events();

        // addr2 is the last subchannel, so no further timer fires.
        env.advance_past_timer().await;
        env.expect_no_events();
    }

    // With a single address, the only connection attempt is on the last
    // subchannel, so no timer is started (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_no_timer_for_single_address() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1"], None);

        env.advance_past_timer().await;
        env.expect_no_events();
    }

    // An earlier connection attempt that succeeds after the timer moved the
    // pass on is selected, and the running timer is cancelled (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_earlier_attempt_ready_cancels_timer() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2", "addr3"], None);
        env.fire_timer().await;
        env.expect_connect_to("addr2");

        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker(&scs[0]);

        // No timer fires: the timer for addr2's attempt was cancelled.
        env.advance_past_timer().await;
        env.expect_no_events();
    }

    // When the current connection attempt fails, the next attempt starts at
    // once with its own Connection Attempt Delay timer; the failed attempt's
    // timer is cancelled (gRFC A61). Ported from Go's
    // TestPickFirstLeaf_HappyEyeballs_TF_ThenTimerFires.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_attempt_failure_restarts_timer() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2", "addr3"], None);

        // addr1 fails partway through its delay; addr2 is attempted at once.
        env.advance_time(CONNECTION_ATTEMPT_DELAY / 2).await;
        env.send_subchannel_update(&scs[0], &SubchannelState::transient_failure("addr1 failed"));
        env.expect_connect_to("addr2");
        env.expect_no_events();

        // Past addr1's original deadline, but before addr2's: nothing fires.
        env.advance_time(CONNECTION_ATTEMPT_DELAY * 3 / 4).await;
        env.expect_no_events();

        // addr2's timer fires, moving the pass on to addr3.
        env.fire_timer().await;
        env.expect_connect_to("addr3");
        env.expect_no_events();
    }

    // Timer work queued behind a READY and then IDLE update is delivered after
    // the LB has returned to Idle. It must not be treated as a connection
    // request.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_stale_timer_work_ignored_in_idle() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2"], None);

        // Hold the timer's work, as if READY and IDLE were queued before it.
        let timer_work = env.expect_timer_work().await;
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker(&scs[0]);
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_request_resolution();
        env.expect_picker_state(ConnectivityState::Idle);

        env.policy.work(timer_work, &mut env.tcc);
        env.expect_no_events();
    }

    // Timer work from an earlier first pass, delivered after a resolver update
    // started a new pass, does not move the new pass on.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_stale_timer_work_ignored_in_new_pass() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1", "addr2"], None);

        // Hold the timer's work, as if the resolver update was handled before
        // it. The new pass reuses the subchannels and attempts addr1 again.
        let timer_work = env.expect_timer_work().await;
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_connect_to("addr1");
        env.expect_no_events();

        env.policy.work(timer_work, &mut env.tcc);
        env.expect_no_events();

        // The new pass's own timer moves it on to addr2.
        env.fire_timer().await;
        env.expect_connect_to("addr2");
        env.expect_no_events();
    }

    // A new pass asks a reused subchannel to connect if it last reported IDLE,
    // even though the previous pass already asked it to. Only the reported
    // state decides whether to connect.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_new_pass_reconnects_reused_idle_subchannel() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1", "addr2"], None);

        // addr1 has not reported CONNECTING when the next update arrives.
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_connect_to("addr1");
        env.expect_no_events();
    }

    // A failure from a subchannel the pass has not reached yet does not count
    // toward ending the pass: the subchannel may be IDLE again by the time the
    // pass reaches it, and is then attempted (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_failure_not_yet_reached_not_counted() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2"], None);

        // addr2 fails and finishes backoff before the pass reaches it.
        env.send_subchannel_update(&scs[1], &SubchannelState::transient_failure("addr2 failed"));
        env.send_subchannel_update(&scs[1], &SubchannelState::idle());

        // addr1 fails; the pass moves on and attempts addr2.
        env.send_subchannel_update(&scs[0], &SubchannelState::transient_failure("addr1 failed"));
        env.expect_connect_to("addr2");
        env.expect_no_events();

        // addr2 fails during this pass, completing it.
        env.send_subchannel_update(&scs[1], &SubchannelState::transient_failure("addr2 again"));
        env.expect_failing_picker("addr2 again");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // If all addresses fail during the first pass, the LB should enter
    // steady state and monitor for backoff expiry to retry connections
    // (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_steady_state_retries() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1"], None);

        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        // Simulate addr1 transitioning to IDLE (backoff over). Should
        // automatically call connect() again.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_connect_to("addr1");
        env.expect_no_events();
    }

    // A subchannel that returns to IDLE during the first pass is not retried
    // until the first pass completes; entering steady state connects it
    // immediately, retries on IDLE, and transitions to READY when a subchannel
    // becomes READY (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_steady_state_multi_backend() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        env.expect_connect_to("addr2");

        // While addr2 is connecting, simulate addr1 going IDLE (backoff over).
        // We should NOT reconnect to addr1 during the first pass.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_no_events();

        // Now fail addr2 to complete first pass. Expect TransientFailure,
        // re-resolution, and Connect(addr1) from first pass exhaustion.
        env.send_subchannel_update(
            &scs[1],
            &SubchannelState::transient_failure("connection refused"),
        );
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();
        env.expect_connect_to("addr1");

        // Simulate addr1 going IDLE again. It should automatically call
        // connect() again.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_connect_to("addr1");

        // Simulate addr1 successfully connecting and becoming READY. The
        // policy should switch to it immediately.
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker(&scs[0]);
        env.expect_no_events();
    }

    // This test is meant to validate that if a new address with different
    // attributes is sent as part of a resolver update, the policy treats it as
    // a different address and creates a new subchannel for it, rather than
    // ignoring it as a duplicate (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_address_update_with_attributes() {
        let addr = "addr1";
        let mut env = new_env();
        let scs = env.simulate_connection(vec![addr], None);

        // Push same address but with attributes.
        let attrs = crate::attributes::Attributes::new().add("metadata_value".to_string());
        let endpoints_updated = create_endpoints(vec![addr], Some(attrs));
        env.send_resolver_update(endpoints_updated).unwrap();

        // This should be a different subchannel due to different attributes.
        let sc = env.expect_new_subchannel_for(addr);
        assert_ne!(sc.address(), scs[0].address());
        env.expect_connect_to(addr);
        env.expect_no_events();
    }

    // If a resolver error is received while the LB is in the process of
    // connecting to addresses, it should not abort the connection attempt or
    // clear the existing addresses, as long as there are still valid addresses
    // in the LB. The error is still returned, so the resolver retries.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_resolver_error_during_connecting() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1"], None);

        let res = env.send_resolver_error("dns resolution failed".to_string());
        assert_eq!(res.unwrap_err(), "dns resolution failed");
        env.expect_no_events();

        // The in-progress connection attempt is still honored.
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker(&scs[0]);
        env.expect_no_events();
    }

    // If an earlier address fails after a later address has already failed
    // during the first pass, the LB should detect that all addresses have
    // failed and report the most recent error (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_happy_eyeballs_out_of_order_failure() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2"], None);

        env.fire_timer().await;
        env.expect_connect_to("addr2");

        // addr2 fails while addr1 is still connecting.
        env.send_subchannel_update(&scs[1], &SubchannelState::transient_failure("addr2 failed"));
        env.expect_no_events();

        env.send_subchannel_update(&scs[0], &SubchannelState::transient_failure("addr1 failed"));
        env.expect_failing_picker("addr1 failed");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // When the current connection attempt fails and no subchannel is left to
    // attempt, the pass cancels the Connection Attempt Delay timer and waits
    // for the earlier attempts to finish (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_attempt_failure_with_nothing_left_cancels_timer() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2", "addr3"], None);

        env.fire_timer().await;
        env.expect_connect_to("addr2");

        // addr3 fails before the pass reaches it, then addr2 fails while addr1
        // is still connecting.
        env.send_subchannel_update(&scs[2], &SubchannelState::transient_failure("addr3 failed"));
        env.send_subchannel_update(&scs[1], &SubchannelState::transient_failure("addr2 failed"));
        env.expect_no_events();

        env.advance_past_timer().await;
        env.expect_no_events();

        // addr1 fails, ending the pass.
        env.send_subchannel_update(&scs[0], &SubchannelState::transient_failure("addr1 failed"));
        env.expect_failing_picker("addr1 failed");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // In steady state, every N failures, where N is the number of subchannels,
    // the policy reports the latest error and requests re-resolution
    // (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_steady_state_reports_every_n_failures() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        env.expect_connect_to("addr2");
        env.send_subchannel_update(&scs[1], &SubchannelState::transient_failure("addr2 failed"));
        env.expect_failing_picker("addr2 failed");
        env.expect_request_resolution();

        // The first failure in steady state is counted, but not reported.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_connect_to("addr1");
        env.send_subchannel_update(&scs[0], &SubchannelState::transient_failure("addr1 again"));
        env.expect_no_events();

        // The second failure reaches N.
        env.send_subchannel_update(&scs[1], &SubchannelState::idle());
        env.expect_connect_to("addr2");
        env.send_subchannel_update(&scs[1], &SubchannelState::transient_failure("addr2 again"));
        env.expect_failing_picker("addr2 again");
        env.expect_request_resolution();
        env.expect_no_events();

        // The count starts over.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_connect_to("addr1");
        env.send_subchannel_update(&scs[0], &SubchannelState::transient_failure("addr1 third"));
        env.expect_no_events();
    }

    // When the selected subchannel disconnects (transitions to Idle), the
    // policy requests re-resolution, reports Idle with an IdlePicker, and on
    // pick reconnects across all addresses (falling over to addr2 if addr1
    // fails) (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_disconnect_to_idle_multi_address_reconnect() {
        let mut env = new_env();
        let scs = env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let reconnected = env.simulate_disconnect_and_reconnect(&scs[0], &["addr1", "addr2"]);

        // Simulate addr1 failing on reconnect. Policy should failover to addr2.
        env.send_subchannel_update(
            &reconnected[0],
            &SubchannelState::transient_failure("connection refused"),
        );
        env.expect_connect_to("addr2");
        env.expect_no_events();
    }

    // Tests that when a resolver update arrives while in FirstPass, any
    // subchannels that have already failed (TransientFailure) preserve their
    // state so the policy does not redundantly re-attempt them before trying
    // remaining addresses (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_resolver_update_preserves_subchannel_failure_state() {
        let mut env = new_env();

        // addr1 fails with TransientFailure. The pass moves on to addr2.
        env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        env.expect_connect_to("addr2");

        // New resolver update [addr1, addr3], retaining addr1, which already
        // failed.
        let endpoints_new = create_endpoints(vec!["addr1", "addr3"], None);
        env.send_resolver_update(endpoints_new).unwrap();

        // addr3 is created as a new subchannel.
        env.expect_new_subchannel_for("addr3");

        // Because addr1 is already in TransientFailure, the policy should skip
        // addr1 and immediately connect addr3.
        env.expect_connect_to("addr3");
        env.expect_no_events();
    }

    // The initial resolver update starts connecting without waiting for
    // exit_idle (gRFC A61: pick_first "will attempt to connect as soon as it
    // receives its initial address list").
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_initial_resolver_update_connects() {
        let mut env = new_env();

        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_new_subchannels(&["addr1", "addr2"]);
        env.expect_connect_to("addr1");
        env.expect_queuing_picker();
        env.expect_no_events();
    }

    // After a disconnect, resolver updates leave the policy IDLE without
    // creating subchannels until exit_idle is called, which connects to the
    // latest address list.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_resolver_update_after_disconnect_stays_idle() {
        let mut env = new_env();
        let scs = env.simulate_successful_connection(vec!["addr1", "addr2"], None);
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_request_resolution();
        env.expect_picker_state(ConnectivityState::Idle);

        let endpoints = create_endpoints(vec!["addr3", "addr4"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_no_events();

        env.policy.exit_idle(&mut env.tcc);
        env.expect_new_subchannels(&["addr3", "addr4"]);
        env.expect_connect_to("addr3");
        env.expect_queuing_picker();
        env.expect_no_events();
    }

    // Tests that when in Transient Failure, receiving a valid resolver update
    // immediately starts connecting without waiting for exit_idle, while
    // continuing to report TRANSIENT_FAILURE until a subchannel is READY
    // (sticky-TF, gRFC A62).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_resolver_update_in_transient_failure_starts_connecting_immediately() {
        let mut env = new_env();

        // Initial resolution failure sets Transient Failure.
        let res = env.send_resolver_error("resolver unavailable".to_string());
        assert_eq!(res.unwrap_err(), "resolver unavailable");
        env.expect_failing_picker("resolver unavailable");
        env.expect_request_resolution();

        // A valid resolver update arrives [addr1, addr2]. Subchannels are
        // created and connection initiates immediately without exit_idle. No
        // CONNECTING picker is reported.
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        let scs = env.expect_new_subchannels(&["addr1", "addr2"]);
        env.expect_connect_to("addr1");
        env.expect_no_events();

        // The policy leaves TRANSIENT_FAILURE once a subchannel is READY.
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker(&scs[0]);
        env.expect_no_events();
    }

    // Tests that exit_idle before any resolver update is ignored, and that
    // the first resolver update then connects and reports CONNECTING.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_ignores_exit_idle_before_resolver_update() {
        let mut env = new_env();
        env.policy.exit_idle(&mut env.tcc);
        env.expect_no_events();

        env.simulate_connection(vec!["addr1", "addr2"], None);
        env.expect_no_events();
    }

    // Tests that during the first pass, updates from subchannels whose
    // addresses were removed by a resolver update are ignored.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_ignores_removed_subchannel_updates_in_first_pass() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2"], None);

        // Replace the address list with [addr3].
        let endpoints = create_endpoints(vec!["addr3"], None);
        env.send_resolver_update(endpoints).unwrap();
        let sc3 = env.expect_new_subchannel_for("addr3");
        env.expect_connect_to("addr3");

        // Updates from the removed subchannels do not select them or advance
        // the first pass.
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.send_subchannel_update(
            &scs[1],
            &SubchannelState::transient_failure("connection refused"),
        );
        env.expect_no_events();

        // The first pass continues with addr3.
        env.send_subchannel_update(&sc3, &SubchannelState::ready());
        env.expect_ready_picker(&sc3);
        env.expect_no_events();
    }

    // Tests that in steady state, updates from subchannels whose addresses
    // were removed by a resolver update are ignored.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_ignores_removed_subchannel_updates_in_steady_state() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1"], None);
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        // Replace the address list with [addr2], and fail addr2 to re-enter
        // steady state. The policy keeps reporting TRANSIENT_FAILURE while
        // connecting (sticky-TF).
        let endpoints = create_endpoints(vec!["addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        let sc2 = env.expect_new_subchannel_for("addr2");
        env.expect_connect_to("addr2");
        env.expect_no_events();
        env.send_subchannel_update(&sc2, &SubchannelState::transient_failure("addr2 failed"));
        env.expect_failing_picker("addr2 failed");
        env.expect_request_resolution();

        // Updates from the removed subchannel do not trigger a connection
        // attempt, select it, or change the picker.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_no_events();

        // Updates from the current subchannel are still handled.
        env.send_subchannel_update(&sc2, &SubchannelState::idle());
        env.expect_connect_to("addr2");
        env.expect_no_events();
    }

    // Tests that updates from a discarded subchannel are ignored during a
    // first pass and in READY, even though its address is still in the
    // address list.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_ignores_stale_subchannel_with_current_address() {
        let mut env = new_env();
        let old = env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let new = env.simulate_disconnect_and_reconnect(&old[0], &["addr1", "addr2"]);

        // A late READY from the discarded addr1 subchannel is ignored.
        env.send_subchannel_update(&old[0], &SubchannelState::ready());
        env.expect_no_events();

        // The new addr1 subchannel is selected.
        env.send_subchannel_update(&new[0], &SubchannelState::ready());
        env.expect_ready_picker(&new[0]);

        // A late IDLE from the discarded addr1 subchannel does not disconnect
        // the selected subchannel.
        env.send_subchannel_update(&old[0], &SubchannelState::idle());
        env.expect_no_events();
    }

    // Tests that in steady state, updates from a discarded subchannel are
    // ignored, even though its address is still in the address list.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_ignores_stale_subchannel_with_current_address_in_steady_state() {
        let mut env = new_env();
        let old = env.simulate_successful_connection(vec!["addr1"], None);

        let new = env.simulate_disconnect_and_reconnect(&old[0], &["addr1"]);

        // The new addr1 subchannel fails, entering steady state.
        env.send_subchannel_update(
            &new[0],
            &SubchannelState::transient_failure("connection refused"),
        );
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        // Updates from the discarded addr1 subchannel do not select it or
        // trigger a connection attempt.
        env.send_subchannel_update(&old[0], &SubchannelState::ready());
        env.send_subchannel_update(&old[0], &SubchannelState::idle());
        env.expect_no_events();
    }

    // Tests that once reporting TRANSIENT_FAILURE after all addresses fail,
    // the policy keeps reporting it across resolver updates and new first
    // passes until a subchannel becomes READY (sticky-TF, gRFC A62).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_sticky_transient_failure_across_resolver_updates() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1"], None);
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        // Re-resolution returns [addr1, addr2]. A new pass starts without
        // reporting CONNECTING. addr1 already failed, so addr2 is attempted.
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        let sc2 = env.expect_new_subchannel_for("addr2");
        env.expect_connect_to("addr2");
        env.expect_no_events();
        env.send_subchannel_update(&sc2, &SubchannelState::connecting());
        env.expect_no_events();

        // Another update during the pass also leaves the reported state
        // unchanged. addr2 reported CONNECTING, so it is not asked to connect
        // again.
        let endpoints = create_endpoints(vec!["addr2", "addr3"], None);
        env.send_resolver_update(endpoints).unwrap();
        let sc3 = env.expect_new_subchannel_for("addr3");
        env.expect_no_events();

        // addr2 fails, and the pass moves on to addr3.
        env.send_subchannel_update(&sc2, &SubchannelState::transient_failure("addr2 failed"));
        env.expect_connect_to("addr3");
        env.expect_no_events();

        // addr3 fails, ending the pass. TRANSIENT_FAILURE is reported again
        // with the latest error.
        env.send_subchannel_update(&sc3, &SubchannelState::transient_failure("addr3 failed"));
        env.expect_failing_picker("addr3 failed");
        env.expect_request_resolution();
        env.expect_no_events();

        // A READY subchannel ends sticky TRANSIENT_FAILURE.
        env.send_subchannel_update(&sc3, &SubchannelState::ready());
        env.expect_ready_picker(&sc3);
        env.expect_no_events();
    }

    // Tests that a pass starting with every subchannel already in
    // TRANSIENT_FAILURE ends at once and reports their connection error
    // (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_pass_starting_in_transient_failure_reports_connection_error() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1"], None);
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        // addr1 is reused and still in TRANSIENT_FAILURE, so the new pass has
        // nothing to attempt.
        let endpoints = create_endpoints(vec!["addr1"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // Tests that a resolver error while reporting TRANSIENT_FAILURE updates the
    // picker with the resolver error, but keeps using the current subchannels.
    // The returned error makes the resolver retry, so no re-resolution is
    // requested.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_resolver_error_in_transient_failure_keeps_subchannels() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1"], None);
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        let res = env.send_resolver_error("dns failure".to_string());
        assert_eq!(res.unwrap_err(), "dns failure");
        env.expect_failing_picker("dns failure");
        env.expect_no_events();

        // The existing subchannel is still retried and can become READY.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_connect_to("addr1");
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker(&scs[0]);
        env.expect_no_events();
    }

    // If new_subchannel returns a subchannel that is already READY, the first
    // pass immediately selects it without connecting earlier subchannels or
    // creating subchannels for later addresses (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_new_subchannel_initially_ready() {
        let mut env = new_env();
        env.send_resolver_update_with_initial_states(
            vec!["addr1", "addr2", "addr3"],
            &[("addr2", SubchannelState::ready())],
        )
        .unwrap();

        let scs = env.expect_new_subchannels(&["addr1", "addr2"]);
        env.expect_ready_picker(&scs[1]);
        env.expect_no_events();
    }

    // If new_subchannel returns a subchannel already in TRANSIENT_FAILURE, the
    // first pass counts it as failed and skips to the next address (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_new_subchannel_initially_transient_failure() {
        let mut env = new_env();
        env.send_resolver_update_with_initial_states(
            vec!["addr1", "addr2"],
            &[(
                "addr1",
                SubchannelState::transient_failure("initial failure"),
            )],
        )
        .unwrap();

        let scs = env.expect_new_subchannels(&["addr1", "addr2"]);
        env.expect_connect_to("addr2");
        env.expect_queuing_picker();
        env.expect_no_events();

        // Once addr2 also fails, the pass finishes without waiting for addr1.
        env.send_subchannel_update(&scs[1], &SubchannelState::transient_failure("addr2 failed"));
        env.expect_failing_picker("addr2 failed");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // If new_subchannel returns a subchannel already in CONNECTING, the first
    // pass waits on it without calling connect(), and advances when the
    // Connection Attempt Delay timer fires (gRFC A61).
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_new_subchannel_initially_connecting() {
        let mut env = new_env();
        env.send_resolver_update_with_initial_states(
            vec!["addr1", "addr2"],
            &[("addr1", SubchannelState::connecting())],
        )
        .unwrap();

        env.expect_new_subchannels(&["addr1", "addr2"]);
        env.expect_queuing_picker();
        env.expect_no_events();

        env.fire_timer().await;
        env.expect_connect_to("addr2");
        env.expect_no_events();
    }
}
