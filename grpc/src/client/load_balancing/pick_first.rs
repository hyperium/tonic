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
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use rand::seq::SliceRandom;

use crate::client::ConnectivityState;
use crate::client::RequestHeaders;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::FailingPicker;
use crate::client::load_balancing::LbConfigJson;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::Pick;
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
            shuffler: build_shuffler(),
            state: PickFirstState::Idle(IdleState {
                addresses: Vec::new(),
            }),
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
    shuffler: Arc<ShufflerFn>,
    state: PickFirstState,
}

impl Debug for PickFirstPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickFirstPolicy")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl PickFirstPolicy {
    fn compile_address(
        &self,
        mut endpoints: Vec<Endpoint>,
        config: &PickFirstConfig,
    ) -> Result<Vec<Address>, String> {
        if config.shuffle_address_list {
            (self.shuffler)(&mut endpoints);
        }

        let mut seen = HashSet::new();
        let mut ipv6 = Vec::new();
        let mut ipv4 = Vec::new();
        let mut unknown = Vec::new();

        for ep in endpoints {
            for addr in ep.addresses {
                if seen.insert(addr.clone()) {
                    if addr.network_type == crate::client::name_resolution::TCP_IP_NETWORK_TYPE {
                        if addr.address.contains(':') {
                            ipv6.push(addr);
                        } else {
                            ipv4.push(addr);
                        }
                    } else {
                        unknown.push(addr);
                    }
                }
            }
        }

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
            controller,
        };
        let current = std::mem::take(&mut self.state);
        self.state = current.subchannel_update(&mut ctx, subchannel, state);
    }
}

// The `PickFirstPolicy` is structured as a discrete finite state machine
// (`PickFirstState`):
// - `Idle`: Initial or post-disconnect state; waits for a pick or `exit_idle`
//   to trigger a connection attempt.
// - `FirstPass`: Happy Eyeballs connection pass staggering connection attempts
//   across resolved addresses.
// - `SteadyState`: All addresses failed; holds sticky TRANSIENT_FAILURE and
//   retries connections as backoffs expire.
// - `Ready`: Successfully connected to a subchannel, routing all picks to it.
impl LbPolicy for PickFirstPolicy {
    type LbConfig = PickFirstConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        let mut ctx = PickFirstContext {
            runtime: &self.runtime,
            work_scheduler: &self.work_scheduler,
            controller,
        };

        match update.endpoints {
            Ok(endpoints) => {
                let addresses = match self.compile_address(endpoints, config) {
                    Ok(addrs) => addrs,
                    Err(e) => {
                        self.state = SteadyState::enter(&mut ctx, Vec::new(), e.clone());
                        return Err(e);
                    }
                };
                let current = std::mem::take(&mut self.state);
                self.state = current.resolver_update(&mut ctx, addresses);
                Ok(())
            }
            Err(error) => {
                let is_idle_without_addresses =
                    matches!(&self.state, PickFirstState::Idle(s) if s.addresses.is_empty());
                let is_tf = matches!(self.state, PickFirstState::SteadyState(_));

                if is_idle_without_addresses || is_tf {
                    self.state = SteadyState::enter(&mut ctx, Vec::new(), error.clone());
                    return Err(error);
                }
                Ok(())
            }
        }
    }

    fn work(&mut self, data: Option<WorkData>, controller: &mut dyn ChannelController) {
        if let Some(data) = data {
            // If data is set, it should be a subchannel update.
            match data.downcast::<SubchannelUpdate>() {
                Ok(update) => {
                    self.subchannel_update(update.subchannel, &update.state, controller);
                }
                Err(data) => debug_assert!(
                    false,
                    "work called with {data:?}; expected a SubchannelUpdate"
                ),
            }
            return;
        }
        let mut ctx = PickFirstContext {
            runtime: &self.runtime,
            work_scheduler: &self.work_scheduler,
            controller,
        };
        let current = std::mem::take(&mut self.state);
        self.state = current.work(&mut ctx);
    }

    fn exit_idle(&mut self, controller: &mut dyn ChannelController) {
        let mut ctx = PickFirstContext {
            runtime: &self.runtime,
            work_scheduler: &self.work_scheduler,
            controller,
        };
        let current = std::mem::take(&mut self.state);
        self.state = current.exit_idle(&mut ctx);
    }
}

struct PickFirstContext<'a> {
    runtime: &'a GrpcRuntime,
    work_scheduler: &'a Arc<dyn WorkScheduler>,
    controller: &'a mut dyn ChannelController,
}

impl PickFirstContext<'_> {
    fn set_failing_picker(&mut self, error: &str) {
        self.controller.update_picker(LbState {
            connectivity_state: ConnectivityState::TransientFailure,
            picker: Arc::new(FailingPicker {
                error: error.to_string(),
            }),
        });
    }
}

// State node: Idle.
#[derive(Debug, Default)]
struct IdleState {
    addresses: Vec<Address>,
}

impl IdleState {
    fn enter(ctx: &mut PickFirstContext<'_>, addresses: Vec<Address>) -> PickFirstState {
        ctx.controller.update_picker(LbState {
            connectivity_state: ConnectivityState::Idle,
            picker: Arc::new(IdlePicker::new(ctx.work_scheduler.clone())),
        });
        PickFirstState::Idle(IdleState { addresses })
    }

    fn resolver_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
    ) -> PickFirstState {
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

    fn work(self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        self.exit_idle(ctx)
    }

    fn exit_idle(self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        FirstPassState::fresh_enter(ctx, self.addresses)
    }
}

// An entry associating a created subchannel handle with its cached
// connectivity state.
#[derive(Debug, Clone)]
struct SubchannelEntry {
    subchannel: Arc<dyn Subchannel>,
    state: SubchannelState,
}

// Returns the entry for the subchannel if it belongs to the current subchannel
// list, or `None` if the subchannel is outdated. Matches by subchannel
// identity, not address: a subchannel from a previous list is outdated even if
// its address is still present.
fn current_entry_mut<'a>(
    entries: &'a mut [SubchannelEntry],
    subchannel: &Arc<dyn Subchannel>,
) -> Option<&'a mut SubchannelEntry> {
    entries
        .iter_mut()
        .find(|e| Arc::ptr_eq(&e.subchannel, subchannel))
}

// Returns the addresses of the subchannels in `entries`, in order.
fn entry_addresses(entries: &[SubchannelEntry]) -> Vec<Address> {
    entries.iter().map(|e| e.subchannel.address()).collect()
}

// State node: FirstPass (Happy Eyeballs).
struct FirstPassState {
    subchannels: Vec<SubchannelEntry>,
    frontier_index: usize,
    // The Happy Eyeballs timer for the current connection attempt. `None`
    // when attempting the last subchannel in the list.
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
                existing.swap_remove(pos)
            } else {
                // Updates for the new subchannel are delivered to our work
                // method via our work scheduler.
                let (subchannel, state) = ctx
                    .controller
                    .new_subchannel(addr, ctx.work_scheduler.clone());
                SubchannelEntry { subchannel, state }
            };

            if entry.state.connectivity_state == ConnectivityState::Ready {
                return ReadyState::enter(ctx, addresses, entry.subchannel);
            }

            new_subchannels.push(entry);
        }

        let mut first_pass = FirstPassState {
            subchannels: new_subchannels,
            frontier_index: 0,
            timer: None,
            last_connection_error: None,
        };

        if let Some(sc) = first_pass.advance_frontier(true) {
            first_pass.trigger_connection(ctx, &sc);

            ctx.controller.update_picker(LbState {
                connectivity_state: ConnectivityState::Connecting,
                picker: Arc::new(QueuingPicker {}),
            });

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

    // Starts a connection attempt on `sc`, the subchannel at the frontier.
    fn trigger_connection(&mut self, ctx: &mut PickFirstContext<'_>, sc: &Arc<dyn Subchannel>) {
        // TODO: Track only subchannel-reported state (do not assume
        // Connecting here) and mark subchannels that have failed in the first
        // pass, to match the Go/C++ implementations (gRFC A61).
        let was_idle = if let Some(entry) = current_entry_mut(&mut self.subchannels, sc) {
            let was_idle = entry.state.connectivity_state == ConnectivityState::Idle;
            entry.state = SubchannelState {
                connectivity_state: ConnectivityState::Connecting,
                last_connection_error: None,
            };
            was_idle
        } else {
            false
        };
        if was_idle {
            sc.connect();
        }
        // Start the Happy Eyeballs timer unless this is the last subchannel in
        // the list (gRFC A61). Replacing the timer cancels any previous one.
        let is_last = self.frontier_index + 1 >= self.subchannels.len();
        self.timer =
            (!is_last).then(|| Timer::start(ctx.runtime.clone(), ctx.work_scheduler.clone()));
    }

    fn advance_frontier(&mut self, reset: bool) -> Option<Arc<dyn Subchannel>> {
        if reset {
            self.frontier_index = 0;
        } else {
            self.frontier_index += 1;
        }

        while self.frontier_index < self.subchannels.len() {
            let entry = &self.subchannels[self.frontier_index];
            match entry.state.connectivity_state {
                ConnectivityState::TransientFailure => self.frontier_index += 1,
                _ => return Some(entry.subchannel.clone()),
            }
        }
        None
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
        let Some(entry) = current_entry_mut(&mut self.subchannels, &subchannel) else {
            // This update is from an outdated subchannel that is no longer in
            // the subchannel list. Ignore it.
            return PickFirstState::FirstPass(self);
        };

        entry.state = state.clone();

        if state.connectivity_state == ConnectivityState::Ready {
            let addresses = entry_addresses(&self.subchannels);
            return ReadyState::enter(ctx, addresses, subchannel);
        }

        if state.connectivity_state == ConnectivityState::TransientFailure {
            if let Some(err) = &state.last_connection_error {
                self.last_connection_error = Some(err.clone());
            }

            if let Some(attempting) = self.subchannels.get(self.frontier_index)
                && Arc::ptr_eq(&attempting.subchannel, &subchannel)
                && let Some(next_sc) = self.advance_frontier(false)
            {
                self.trigger_connection(ctx, &next_sc);
            }
        }

        if self.frontier_index >= self.subchannels.len() {
            // TODO: End the first pass once every subchannel is marked as
            // failed, rather than when none are Connecting, to match the Go/C++
            // implementations.
            let any_connecting = self
                .subchannels
                .iter()
                .any(|e| e.state.connectivity_state == ConnectivityState::Connecting);

            if !any_connecting {
                return self.enter_steady_state(ctx);
            }
        }

        PickFirstState::FirstPass(self)
    }

    fn work(mut self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        if self.timer.as_ref().is_some_and(Timer::expired) {
            self.timer = None;
            if let Some(next_sc) = self.advance_frontier(false) {
                self.trigger_connection(ctx, &next_sc);
            }
        }
        PickFirstState::FirstPass(self)
    }
}

// State node: SteadyState (Sticky TRANSIENT_FAILURE).
#[derive(Debug)]
struct SteadyState {
    subchannels: Vec<SubchannelEntry>,
    failure_threshold: usize,
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
            failure_threshold: subchannels.len(),
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
        let Some(entry) = current_entry_mut(&mut self.subchannels, &subchannel) else {
            // This update is from an outdated subchannel that is no longer in
            // the subchannel list. Ignore it.
            return PickFirstState::SteadyState(self);
        };
        entry.state = state.clone();

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
                    ctx.set_failing_picker(&self.last_connection_error);
                }
                self.failure_count += 1;
                if self.failure_count >= self.failure_threshold {
                    self.failure_count = 0;
                    ctx.controller.request_resolution();
                }
                PickFirstState::SteadyState(self)
            }
            ConnectivityState::Connecting => PickFirstState::SteadyState(self),
        }
    }
}

// State node: Ready.
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
        ctx.controller.update_picker(LbState {
            connectivity_state: ConnectivityState::Ready,
            picker: Arc::new(OneSubchannelPicker {
                sc: selected.clone(),
            }),
        });
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
            IdleState::enter(ctx, self.addresses)
        } else {
            PickFirstState::Ready(self)
        }
    }
}

// The Pick First State Machine enum.
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

    fn work(self, ctx: &mut PickFirstContext<'_>) -> PickFirstState {
        match self {
            Self::Idle(s) => s.work(ctx),
            Self::FirstPass(s) => s.work(ctx),
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

impl Debug for PickFirstState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle(s) => f
                .debug_struct("Idle")
                .field("addresses", &s.addresses)
                .finish(),
            Self::FirstPass(fp) => f
                .debug_struct("FirstPass")
                .field("frontier_index", &fp.frontier_index)
                .field("subchannels", &fp.subchannels)
                .finish(),
            Self::SteadyState(ss) => f.debug_tuple("SteadyState").field(ss).finish(),
            Self::Ready(r) => f
                .debug_struct("Ready")
                .field("selected", &r.selected.address())
                .finish(),
        }
    }
}

// Implements the happy eyeballs timer task. `expired` becomes set when it
// fires. When dropped, the timer is cancelled.
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
            runtime.sleep(std::time::Duration::from_millis(250)).await;
            expired_clone.store(true, Ordering::SeqCst);
            work_scheduler.schedule_work(None);
        }));
        Self { expired, handle }
    }

    // Returns whether the timer has expired yet.
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
    fn pick(&self, _: &RequestHeaders) -> PickResult {
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

#[cfg(test)]
mod test {
    use std::time::Duration;

    use super::*;
    use crate::StatusCodeError;
    use crate::client::load_balancing::test_utils::TestEnv;
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
        // addresses. Returns the subchannels created, in address order. Does
        // not imply that the connection succeeded or failed.
        fn simulate_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) -> Vec<Arc<dyn Subchannel>> {
            let endpoints = create_endpoints(addrs.clone(), attrs);
            self.send_resolver_update(endpoints).unwrap();
            self.expect_picker_state(ConnectivityState::Idle);

            self.policy.exit_idle(&mut self.tcc);

            let subchannels: Vec<_> = addrs
                .iter()
                .map(|addr| {
                    let sc = self.expect_new_subchannel();
                    assert_eq!(sc.address().address.to_string(), *addr);
                    sc
                })
                .collect();

            self.expect_connect_to(addrs[0]);
            self.expect_picker_state(ConnectivityState::Connecting);
            subchannels
        }

        // Simulates a connection where the first address becomes READY, and
        // consumes the resulting READY picker update.
        fn simulate_successful_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) -> Vec<Arc<dyn Subchannel>> {
            let first = addrs[0];
            let subchannels = self.simulate_connection(addrs, attrs);
            self.send_subchannel_update(&subchannels[0], &SubchannelState::ready());
            self.expect_ready_picker(first);
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

        // Disconnects the selected subchannel, then triggers a reconnect with
        // an RPC, which replaces every subchannel. `addrs` is the address
        // list, starting with the selected subchannel's address. Returns the
        // new subchannels, in address order.
        fn simulate_disconnect_and_reconnect(
            &mut self,
            selected: &Arc<dyn Subchannel>,
            addrs: &[&str],
        ) -> Vec<Arc<dyn Subchannel>> {
            self.send_subchannel_update(selected, &SubchannelState::idle());
            let picker = self.expect_picker_state(ConnectivityState::Idle);
            assert!(matches!(
                picker.pick(&RequestHeaders::default()),
                PickResult::Queue
            ));
            let data = self.expect_schedule_work();
            self.policy.work(data, &mut self.tcc);
            let subchannels: Vec<_> = addrs
                .iter()
                .map(|addr| {
                    let sc = self.expect_new_subchannel();
                    assert_eq!(sc.address().address.to_string(), *addr);
                    sc
                })
                .collect();
            self.expect_connect_to(addrs[0]);
            self.expect_picker_state(ConnectivityState::Connecting);
            subchannels
        }

        // Verifies that the policy asked the subchannel for `addr` to connect.
        fn expect_connect_to(&mut self, addr: &str) {
            assert_eq!(self.expect_connect().address.to_string(), addr);
        }

        // Verifies that the policy produced a picker in `want` state, and
        // returns it.
        fn expect_picker_state(&mut self, want: ConnectivityState) -> Arc<dyn Picker> {
            let state = self.expect_picker_update();
            assert_eq!(state.connectivity_state, want);
            state.picker
        }

        // Verifies that the policy produced a READY picker that picks the
        // subchannel for `addr`, and returns that subchannel.
        fn expect_ready_picker(&mut self, addr: &str) -> Arc<dyn Subchannel> {
            let picker = self.expect_picker_state(ConnectivityState::Ready);
            match picker.pick(&RequestHeaders::default()) {
                PickResult::Pick(pick) => {
                    assert_eq!(pick.subchannel.address().address.to_string(), addr);
                    pick.subchannel
                }
                other => panic!("unexpected pick result {other:?}"),
            }
        }

        // Verifies that the policy produced a TRANSIENT_FAILURE picker that
        // fails picks with UNAVAILABLE and `error`.
        fn expect_failing_picker(&mut self, error: &str) {
            let picker = self.expect_picker_state(ConnectivityState::TransientFailure);
            match picker.pick(&RequestHeaders::default()) {
                PickResult::Fail(status) => {
                    assert_eq!(status.code(), StatusCodeError::Unavailable);
                    assert_eq!(status.message(), error);
                }
                other => panic!("unexpected pick result {other:?}"),
            }
        }

        // Advances the paused tokio clock past the 250ms Happy Eyeballs delay,
        // letting any pending timer fire.
        async fn advance_past_timer(&mut self) {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        // Lets the Happy Eyeballs timer fire and delivers the resulting work
        // to the policy. Requires a paused tokio clock.
        async fn fire_timer(&mut self) {
            self.advance_past_timer().await;
            let data = self.expect_schedule_work();
            assert!(data.is_none());
            self.policy.work(data, &mut self.tcc);
        }
    }

    // The LB can successfully connect to the first address, and updates the
    // picker to READY with the correct subchannel.
    #[tokio::test]
    async fn test_pick_first_basic_connection() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2"], None);

        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker("addr1");
        env.expect_no_events();
    }

    // If the first address fails, the LB should failover to the second address.
    #[tokio::test]
    async fn test_pick_first_failover() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        env.expect_connect_to("addr2");

        env.send_subchannel_update(&scs[1], &SubchannelState::ready());
        env.expect_ready_picker("addr2");
        env.expect_no_events();
    }

    // Ensures that if a subchannel is already selected, and is still present in
    // the new resolver update, the LB will keep using it and not switch to a
    // different subchannel.
    #[tokio::test]
    async fn test_pick_first_stickiness() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let endpoints_new = create_endpoints(vec!["addr2", "addr1", "addr3"], None);
        env.send_resolver_update(endpoints_new).unwrap();

        // No new subchannels, connects, or picker updates: the READY picker
        // for addr1 remains in use.
        env.expect_no_events();
    }

    // If all addresses fail during a connection pass, the LB should update to
    // TransientFailure and request re-resolution.
    #[tokio::test]
    async fn test_pick_first_exhaustion() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1"], None);

        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // Shuffling and interleaving of addresses is deterministic and correct
    // based on the provided shuffler and config.
    #[tokio::test]
    async fn test_pick_first_shuffling_and_interleaving_deterministic() {
        const NUM_ADDRS: usize = 4;
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
        env.expect_picker_state(ConnectivityState::Idle);

        env.policy.exit_idle(&mut env.tcc);

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

        env.expect_connect_to("::2");
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
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
        env.expect_picker_state(ConnectivityState::Idle);

        env.policy.exit_idle(&mut env.tcc);

        // Should only create subchannels for addr1 and addr2 (2 unique addrs).
        let sc1 = env.expect_new_subchannel();
        assert_eq!(sc1.address().address.to_string(), "addr1");
        let sc2 = env.expect_new_subchannel();
        assert_eq!(sc2.address().address.to_string(), "addr2");
        env.expect_connect_to("addr1");
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
    }

    // If the resolver update contains no addresses, the LB should clear
    // subchannels, update to TransientFailure, and request re-resolution.
    #[tokio::test]
    async fn test_pick_first_empty_update_clears_state() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let res = env.send_resolver_update(vec![]);
        assert!(res.is_err());

        env.expect_failing_picker("empty address list");
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // If the timer expires during a connection pass, the LB should advance to
    // the next subchannel and trigger a connection attempt. No timer is
    // started for the last subchannel.
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
    // subchannel, so no timer is started.
    #[tokio::test(start_paused = true)]
    async fn test_pick_first_no_timer_for_single_address() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1"], None);

        env.advance_past_timer().await;
        env.expect_no_events();
    }

    // If all addresses fail during a connection pass, the LB should enter
    // steady state and monitor for backoff expiry to retry connections.
    #[tokio::test]
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

    // If the LB is in steady state, and a new address becomes ready, it should
    // switch to it immediately. If the current active address goes idle, it
    // should trigger a retry, but should not switch back to it until it reports
    // ready.
    #[tokio::test]
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
        env.expect_ready_picker("addr1");
        env.expect_no_events();
    }

    // If the LB is in steady state, and all addresses fail, it should trigger a
    // re-resolution. If one of the addresses goes idle during this time, it
    // should trigger an immediate connection attempt, rather than waiting for
    // the timer. This prevents the load balancer from getting stuck in idle if
    // all addresses fail at the same time.
    #[tokio::test]
    async fn test_pick_first_steady_state_stuck_idle_prevention() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        env.expect_connect_to("addr2");

        // Simulate addr1 backing off and transitioning to IDLE early (while
        // addr2 is still connecting). Expect no events yet because first pass
        // is still active.
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        env.expect_no_events();

        // Fail addr2 to exhaust the first pass.
        env.send_subchannel_update(
            &scs[1],
            &SubchannelState::transient_failure("connection refused"),
        );
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        // Expect an immediate Connect(addr1) event triggered by the exhaustion
        // loop sweeping up the early IDLE subchannel.
        env.expect_connect_to("addr1");
        env.expect_no_events();
    }

    // This test is meant to validate that if a new address with different
    // attributes is sent as part of a resolver update, the policy treats it as
    // a different address and creates a new subchannel for it, rather than
    // ignoring it as a duplicate.
    #[tokio::test]
    async fn test_pick_first_address_update_with_attributes() {
        let addr = "addr1";
        let mut env = new_env();
        let scs = env.simulate_connection(vec![addr], None);

        // Push same address but with attributes.
        let attrs = crate::attributes::Attributes::new().add("metadata_value".to_string());
        let endpoints_updated = create_endpoints(vec![addr], Some(attrs));
        env.send_resolver_update(endpoints_updated).unwrap();

        // This should be a different subchannel due to different attributes.
        let sc = env.expect_new_subchannel();
        assert_eq!(sc.address().address.to_string(), addr);
        assert_ne!(sc.address(), scs[0].address());
        env.expect_connect_to(addr);
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
    }

    // If a resolver error is received while the LB is in the process of
    // connecting to addresses, it should not abort the connection attempt or
    // clear the existing addresses, as long as there are still valid addresses
    // in the LB.
    #[tokio::test]
    async fn test_pick_first_resolver_error_during_connecting() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1"], None);

        env.send_resolver_error("dns resolution failed".to_string())
            .unwrap();
        env.expect_no_events();

        // The in-progress connection attempt is still honored.
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.expect_ready_picker("addr1");
        env.expect_no_events();
    }

    // If an earlier address fails after a later address has already failed
    // during the first pass, the LB should detect that all addresses have
    // failed and report the most recent error.
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

    // In steady state, each new subchannel failure updates the failing picker
    // with the most recent connection error.
    #[tokio::test]
    async fn test_pick_first_steady_state_freshest_error() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1"], None);

        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        env.send_subchannel_update(
            &scs[0],
            &SubchannelState::transient_failure("steady state network drop"),
        );
        env.expect_failing_picker("steady state network drop");
        // Every subchannel has failed again, so re-resolution is requested.
        env.expect_request_resolution();
        env.expect_no_events();
    }

    // Tests that when a selected subchannel disconnects (transitions to Idle),
    // the policy reports an Idle state and uses an IdlePicker.
    #[tokio::test]
    async fn test_pick_first_disconnect_to_idle_and_reconnect() {
        let mut env = new_env();
        let scs = env.simulate_successful_connection(vec!["addr1"], None);

        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        let idle_picker = env.expect_picker_state(ConnectivityState::Idle);
        env.expect_no_events();

        let pick_result = idle_picker.pick(&RequestHeaders::default());
        assert!(matches!(pick_result, PickResult::Queue));

        let data = env.expect_schedule_work();
        assert!(data.is_none());
        env.policy.work(data, &mut env.tcc);

        let sc = env.expect_new_subchannel();
        assert_eq!(sc.address().address.to_string(), "addr1");
        env.expect_connect_to("addr1");
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
    }

    // Tests that when connected to multi-address endpoints, if the winner drops
    // to Idle and reconnects on RPC, the policy attempts to connect to all
    // original addresses (falling over to addr2 if addr1 fails).
    #[tokio::test]
    async fn test_pick_first_disconnect_to_idle_multi_address_reconnect() {
        let mut env = new_env();
        let scs = env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        // Simulate addr1 disconnecting (transitioning to Idle).
        env.send_subchannel_update(&scs[0], &SubchannelState::idle());
        let idle_picker = env.expect_picker_state(ConnectivityState::Idle);
        env.expect_no_events();

        // Simulate an RPC happening.
        let pick_result = idle_picker.pick(&RequestHeaders::default());
        assert!(matches!(pick_result, PickResult::Queue));
        let data = env.expect_schedule_work();
        assert!(data.is_none());

        // Call work to execute the scheduled connection attempt.
        env.policy.work(data, &mut env.tcc);

        // Reconnect creates subchannels for both addr1 and addr2, and
        // initiates reconnection to addr1.
        let sc1 = env.expect_new_subchannel();
        assert_eq!(sc1.address().address.to_string(), "addr1");
        let sc2 = env.expect_new_subchannel();
        assert_eq!(sc2.address().address.to_string(), "addr2");
        env.expect_connect_to("addr1");
        env.expect_picker_state(ConnectivityState::Connecting);

        // Simulate addr1 failing on reconnect. Policy should failover to addr2.
        env.send_subchannel_update(
            &sc1,
            &SubchannelState::transient_failure("connection refused"),
        );
        env.expect_connect_to("addr2");
        env.expect_no_events();
    }

    // Tests that when a resolver update arrives while in FirstPass, any
    // subchannels that have already failed (TransientFailure) preserve their
    // state so the policy does not redundantly re-attempt them before trying
    // remaining addresses.
    #[tokio::test]
    async fn test_pick_first_resolver_update_preserves_subchannel_failure_state() {
        let mut env = new_env();

        // addr1 fails with TransientFailure. Frontier advances to addr2.
        env.simulate_failed_connection(vec!["addr1", "addr2"], None);
        env.expect_connect_to("addr2");

        // New resolver update [addr1, addr3], retaining addr1, which already
        // failed.
        let endpoints_new = create_endpoints(vec!["addr1", "addr3"], None);
        env.send_resolver_update(endpoints_new).unwrap();

        // addr3 is created as a new subchannel.
        let sc3 = env.expect_new_subchannel();
        assert_eq!(sc3.address().address.to_string(), "addr3");

        // Because addr1 is already in TransientFailure, the policy should skip
        // addr1 and immediately connect addr3.
        env.expect_connect_to("addr3");
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
    }

    // Tests that when in IDLE, receiving resolver updates leaves the policy in
    // IDLE without creating subchannels or connecting until exit_idle is
    // called.
    #[tokio::test]
    async fn test_pick_first_idle_on_resolver_update_until_exit_idle() {
        let mut env = new_env();

        // Send initial resolver update [addr1, addr2]. Expect IDLE picker
        // update and NO subchannels or connect attempts.
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_picker_state(ConnectivityState::Idle);
        env.expect_no_events();

        // Send second resolver update [addr3, addr4] while still in IDLE.
        let endpoints2 = create_endpoints(vec!["addr3", "addr4"], None);
        env.send_resolver_update(endpoints2).unwrap();
        env.expect_picker_state(ConnectivityState::Idle);
        env.expect_no_events();

        // Explicitly exit idle. Subchannels are now created for the latest
        // address list [addr3, addr4] and connection begins.
        env.policy.exit_idle(&mut env.tcc);
        let sc3 = env.expect_new_subchannel();
        assert_eq!(sc3.address().address.to_string(), "addr3");
        let sc4 = env.expect_new_subchannel();
        assert_eq!(sc4.address().address.to_string(), "addr4");
        env.expect_connect_to("addr3");
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
    }

    // Tests that when in Transient Failure, receiving a valid resolver update
    // immediately transitions to connecting without waiting for exit_idle.
    #[tokio::test]
    async fn test_pick_first_resolver_update_in_transient_failure_starts_connecting_immediately() {
        let mut env = new_env();

        // Initial resolution failure sets Transient Failure.
        let res = env.send_resolver_error("resolver unavailable".to_string());
        assert!(res.is_err());
        env.expect_failing_picker("resolver unavailable");
        env.expect_request_resolution();

        // A valid resolver update arrives [addr1, addr2]. Subchannels are
        // created and connection initiates immediately without exit_idle.
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_new_subchannel();
        env.expect_new_subchannel();
        env.expect_connect_to("addr1");
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
    }

    // Tests that calling exit_idle on an empty IdleState transitions to
    // Transient Failure, requests resolution, and immediately starts
    // connecting once valid endpoints arrive.
    #[tokio::test]
    async fn test_pick_first_exit_idle_with_empty_addresses_transitions_to_transient_failure_and_recovers()
     {
        let mut env = new_env();

        // Calling exit_idle before any resolver update.
        env.policy.exit_idle(&mut env.tcc);
        env.expect_failing_picker("all addresses in transient failure");
        env.expect_request_resolution();

        // Name resolution completes with [addr1, addr2]. Subchannels are
        // created and connection initiates immediately.
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        env.expect_new_subchannel();
        env.expect_new_subchannel();
        env.expect_connect_to("addr1");
        env.expect_picker_state(ConnectivityState::Connecting);
        env.expect_no_events();
    }

    // Tests that during a connection pass, updates from subchannels whose
    // addresses were removed by a resolver update are ignored.
    #[tokio::test]
    async fn test_pick_first_ignores_removed_subchannel_updates_in_first_pass() {
        let mut env = new_env();
        let scs = env.simulate_connection(vec!["addr1", "addr2"], None);

        // Replace the address list with [addr3].
        let endpoints = create_endpoints(vec!["addr3"], None);
        env.send_resolver_update(endpoints).unwrap();
        let sc3 = env.expect_new_subchannel();
        assert_eq!(sc3.address().address.to_string(), "addr3");
        env.expect_connect_to("addr3");
        env.expect_picker_state(ConnectivityState::Connecting);

        // Updates from the removed subchannels do not select them or advance
        // the connection pass.
        env.send_subchannel_update(&scs[0], &SubchannelState::ready());
        env.send_subchannel_update(
            &scs[1],
            &SubchannelState::transient_failure("connection refused"),
        );
        env.expect_no_events();

        // The connection pass continues with addr3.
        env.send_subchannel_update(&sc3, &SubchannelState::ready());
        env.expect_ready_picker("addr3");
        env.expect_no_events();
    }

    // Tests that in steady state, updates from subchannels whose addresses
    // were removed by a resolver update are ignored.
    #[tokio::test]
    async fn test_pick_first_ignores_removed_subchannel_updates_in_steady_state() {
        let mut env = new_env();
        let scs = env.simulate_failed_connection(vec!["addr1"], None);
        env.expect_failing_picker("connection refused");
        env.expect_request_resolution();

        // Replace the address list with [addr2], and fail addr2 to re-enter
        // steady state.
        let endpoints = create_endpoints(vec!["addr2"], None);
        env.send_resolver_update(endpoints).unwrap();
        let sc2 = env.expect_new_subchannel();
        assert_eq!(sc2.address().address.to_string(), "addr2");
        env.expect_connect_to("addr2");
        env.expect_picker_state(ConnectivityState::Connecting);
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
    // connection pass and in READY, even though its address is still in the
    // address list.
    #[tokio::test]
    async fn test_pick_first_ignores_stale_subchannel_with_current_address() {
        let mut env = new_env();
        let old = env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let new = env.simulate_disconnect_and_reconnect(&old[0], &["addr1", "addr2"]);

        // A late READY from the discarded addr1 subchannel is ignored.
        env.send_subchannel_update(&old[0], &SubchannelState::ready());
        env.expect_no_events();

        // The new addr1 subchannel is selected.
        env.send_subchannel_update(&new[0], &SubchannelState::ready());
        let selected = env.expect_ready_picker("addr1");
        assert!(Arc::ptr_eq(&selected, &new[0]));

        // A late IDLE from the discarded addr1 subchannel does not disconnect
        // the selected subchannel.
        env.send_subchannel_update(&old[0], &SubchannelState::idle());
        env.expect_no_events();
    }

    // Tests that in steady state, updates from a discarded subchannel are
    // ignored, even though its address is still in the address list.
    #[tokio::test]
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
}
