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

/// A load balancing policy that receives endpoints from the name resolver and
/// connects to the first available backend using the [Happy Eyeballs](https://datatracker.ietf.org/doc/html/rfc8305)
/// connection algorithm.
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
// - `Idle`: Initial state or post-disconnect state waiting for traffic to trigger
//   resolution.
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
                        self.state =
                            SteadyState::enter(&mut ctx, Vec::new(), Vec::new(), e.clone());
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
                    self.state =
                        SteadyState::enter(&mut ctx, Vec::new(), Vec::new(), error.clone());
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

    #[allow(clippy::unused_self)]
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

// An entry associating an address, its created subchannel handle, and its cached
// connectivity state.
#[derive(Debug, Clone)]
struct SubchannelEntry {
    address: Address,
    subchannel: Arc<dyn Subchannel>,
    state: SubchannelState,
}

/// Returns the entry for the subchannel if its address is present in the most
/// recently received address list, or `None` if the subchannel is outdated.
fn current_entry_mut<'a>(
    entries: &'a mut [SubchannelEntry],
    subchannel: &Arc<dyn Subchannel>,
) -> Option<&'a mut SubchannelEntry> {
    let addr = subchannel.address();
    entries.iter_mut().find(|e| e.address == addr)
}

// State node: FirstPass (Happy Eyeballs).
struct FirstPassState {
    addresses: Vec<Address>,
    subchannels: Vec<SubchannelEntry>,
    frontier_index: usize,
    timer: Timer,
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
            let entry = if let Some(pos) = existing.iter().position(|e| &e.address == addr) {
                existing.swap_remove(pos)
            } else {
                // Updates for the new subchannel are delivered to our work
                // method via our work scheduler.
                // TODO: handle updates arriving before the call returns
                let (subchannel, state) = ctx
                    .controller
                    .new_subchannel(addr, ctx.work_scheduler.clone());
                SubchannelEntry {
                    address: addr.clone(),
                    subchannel,
                    state,
                }
            };

            if entry.state.connectivity_state == ConnectivityState::Ready {
                return ReadyState::enter(ctx, addresses, entry.subchannel);
            }

            new_subchannels.push(entry);
        }

        let mut first_pass = FirstPassState {
            addresses,
            subchannels: new_subchannels,
            frontier_index: 0,
            timer: Timer::start(ctx.runtime.clone(), ctx.work_scheduler.clone()),
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
        SteadyState::enter(ctx, self.addresses, self.subchannels, err)
    }

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
        self.timer = Timer::start(ctx.runtime.clone(), ctx.work_scheduler.clone());
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
        let addr = subchannel.address();

        let Some(entry) = current_entry_mut(&mut self.subchannels, &subchannel) else {
            // This update is from an outdated subchannel that is no longer in
            // the address list. Ignore it.
            return PickFirstState::FirstPass(self);
        };

        entry.state = state.clone();

        if state.connectivity_state == ConnectivityState::Ready {
            return ReadyState::enter(ctx, self.addresses, subchannel);
        }

        if state.connectivity_state == ConnectivityState::TransientFailure {
            if let Some(err) = &state.last_connection_error {
                self.last_connection_error = Some(err.clone());
            }

            if let Some(attempting) = self.subchannels.get(self.frontier_index)
                && attempting.address == addr
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
        if self.timer.expired()
            && let Some(next_sc) = self.advance_frontier(false)
        {
            self.trigger_connection(ctx, &next_sc);
        }
        PickFirstState::FirstPass(self)
    }
}

// State node: SteadyState (Sticky TRANSIENT_FAILURE).
#[derive(Debug)]
struct SteadyState {
    addresses: Vec<Address>,
    subchannels: Vec<SubchannelEntry>,
    failure_threshold: usize,
    failure_count: usize,
    last_connection_error: String,
}

impl SteadyState {
    fn enter(
        ctx: &mut PickFirstContext<'_>,
        addresses: Vec<Address>,
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
            addresses,
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
            // the address list. Ignore it.
            return PickFirstState::SteadyState(self);
        };
        entry.state = state.clone();

        match state.connectivity_state {
            ConnectivityState::Ready => ReadyState::enter(ctx, self.addresses, subchannel),
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

    #[allow(clippy::needless_pass_by_value)]
    fn subchannel_update(
        self,
        ctx: &mut PickFirstContext<'_>,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) -> PickFirstState {
        let addr = subchannel.address();
        if self.selected.address() == addr && state.connectivity_state != ConnectivityState::Ready {
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
                .field("addresses", &fp.addresses)
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
    use super::*;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::client::load_balancing::test_utils::TestEvent;
    use crate::rt::default_runtime;

    impl PickFirstPolicy {
        fn selected(&self) -> Option<&Arc<dyn Subchannel>> {
            match &self.state {
                PickFirstState::Ready(r) => Some(&r.selected),
                _ => None,
            }
        }

        fn timer(&self) -> Option<&Timer> {
            match &self.state {
                PickFirstState::FirstPass(fp) => Some(&fp.timer),
                _ => None,
            }
        }

        fn steady_state(&self) -> Option<&SteadyState> {
            match &self.state {
                PickFirstState::SteadyState(ss) => Some(ss),
                _ => None,
            }
        }

        fn subchannels(&self) -> Vec<Arc<dyn Subchannel>> {
            match &self.state {
                PickFirstState::FirstPass(fp) => fp
                    .subchannels
                    .iter()
                    .map(|e| e.subchannel.clone())
                    .collect(),
                PickFirstState::SteadyState(ss) => ss
                    .subchannels
                    .iter()
                    .map(|e| e.subchannel.clone())
                    .collect(),
                _ => Vec::new(),
            }
        }

        fn last_connection_error(&self) -> Option<&str> {
            match &self.state {
                PickFirstState::FirstPass(fp) => fp.last_connection_error.as_deref(),
                PickFirstState::SteadyState(ss) => Some(&ss.last_connection_error),
                _ => None,
            }
        }
    }

    // Helper to create endpoints from a list of address strings.
    // If attrs are provided, they will be added to each endpoint; otherwise,
    // default attributes will be used.
    #[allow(clippy::needless_pass_by_value)]
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
        // addresses. The resulting events can be inspected via the test
        // environment. Does not imply that the connection succeeded or failed.
        fn simulate_connection(
            &mut self,
            addrs: Vec<&str>,
            attrs: Option<crate::attributes::Attributes>,
        ) {
            let addrs_len = addrs.len();
            let endpoints = create_endpoints(addrs, attrs);
            self.send_resolver_update(endpoints).unwrap();

            let state = self.expect_picker_update();
            assert_eq!(state.connectivity_state, ConnectivityState::Idle);

            self.policy.exit_idle(&mut self.tcc);

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
            let sc1 = self.policy.subchannels()[0].clone();
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
            let sc1 = self.policy.subchannels()[0].clone();
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
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        // Should update picker to READY with sc1.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let res = state.picker.pick(&RequestHeaders::default());
        match res {
            PickResult::Pick(pick) => {
                assert_eq!(pick.subchannel.address().address.to_string(), "addr1");
            }
            other => panic!("unexpected pick result {other:?}"),
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
        let sc2 = env.policy.subchannels()[1].clone();
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

        // Should NOT have any new subchannels or connect events because it stuck to the
        // original selected subchannel.
        env.expect_no_events();

        assert_eq!(
            env.policy
                .selected()
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

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Idle);

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

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Idle);

        env.policy.exit_idle(&mut env.tcc);

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

        assert_eq!(env.policy.subchannels().len(), 2, "De-duplication failed");
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
            .timer()
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
        let sc1 = env.policy.subchannels()[0].clone();

        // Expect UpdatePicker(TransientFailure) and RequestResolution.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();

        // Ensure steady state was entered.
        assert!(env.policy.steady_state().is_some());

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
        let sc1 = env.policy.subchannels()[0].clone();

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
        env.expect_no_events();

        // Now fail addr2 to complete first pass.
        let sc2 = env.policy.subchannels()[1].clone();
        env.send_subchannel_update(
            &sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("connection refused".to_string()),
            },
        );

        // Expect UpdatePicker(TransientFailure), RequestResolution, and Connect(addr1)
        // from first pass exhaustion.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");

        // Confirm LB is in steady state.
        assert!(env.policy.steady_state().is_some());

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
        let res = state.picker.pick(&RequestHeaders::default());
        match res {
            PickResult::Pick(pick) => {
                assert_eq!(pick.subchannel.address().address.to_string(), "addr1");
            }
            other => panic!("unexpected pick result {other:?}"),
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
        let sc1 = env.policy.subchannels()[0].clone();

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
        let sc2 = env.policy.subchannels()[1].clone();
        env.send_subchannel_update(
            &sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("connection refused".to_string()),
            },
        );

        // Expect UpdatePicker(TransientFailure) and RequestResolution from exhaustion.
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
    // in the LB.
    #[tokio::test]
    async fn test_pick_first_resolver_error_during_connecting() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1"], None);

        // Simulate resolver error arriving.
        let resolver_error = "dns resolution failed".to_string();
        env.send_resolver_error(resolver_error).unwrap();

        env.expect_no_events();

        assert!(
            !env.policy.subchannels().is_empty(),
            "Subchannels erroneously cleared by resolver error."
        );
    }

    // Out-of-Order Failure Detection.
    #[tokio::test]
    async fn test_pick_first_happy_eyeballs_out_of_order_failure() {
        let mut env = new_env();
        env.simulate_connection(vec!["addr1", "addr2"], None);

        env.policy
            .timer()
            .as_ref()
            .unwrap()
            .expired
            .store(true, Ordering::SeqCst);
        env.policy.work(None, &mut env.tcc);

        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");

        let sc2 = env.policy.subchannels()[1].clone();
        env.send_subchannel_update(
            &sc2,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("addr2 failed".to_string()),
            },
        );

        env.expect_no_events();

        let sc1 = env.policy.subchannels()[0].clone();
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

    // Freshest Error Caching (Steady State).
    #[tokio::test]
    async fn test_pick_first_steady_state_freshest_error() {
        let mut env = new_env();
        env.simulate_failed_connection(vec!["addr1"], None);

        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();
        assert!(env.policy.steady_state().is_some());

        let sc1 = env.policy.subchannels()[0].clone();
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("steady state network drop".to_string()),
            },
        );

        assert_eq!(
            env.policy.last_connection_error(),
            Some("steady state network drop")
        );
    }

    // Tests that when a selected subchannel disconnects (transitions to Idle),
    // the policy reports an Idle state and uses an IdlePicker.
    #[tokio::test]
    async fn test_pick_first_disconnect_to_idle_and_reconnect() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1"], None);

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let res = state.picker.pick(&RequestHeaders::default());
        let sc1 = match res {
            PickResult::Pick(pick) => {
                assert_eq!(pick.subchannel.address().address.to_string(), "addr1");
                pick.subchannel
            }
            other => panic!("unexpected pick result {other:?}"),
        };

        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
        );

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Idle);
        let idle_picker = state.picker;

        env.expect_no_events();

        let pick_result = idle_picker.pick(&RequestHeaders::default());
        assert!(matches!(pick_result, PickResult::Queue));

        let data = env.expect_schedule_work();
        assert!(data.is_none());

        env.policy.work(data, &mut env.tcc);

        env.expect_new_subchannel();
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
    }

    // Tests that when connected to multi-address endpoints, if the winner drops
    // to Idle and reconnects on RPC, the policy attempts to connect to all
    // original addresses (falling over to addr2 if addr1 fails).
    #[tokio::test]
    async fn test_pick_first_disconnect_to_idle_multi_address_reconnect() {
        let mut env = new_env();
        env.simulate_successful_connection(vec!["addr1", "addr2"], None);

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Ready);
        let res = state.picker.pick(&RequestHeaders::default());
        let sc1 = match res {
            PickResult::Pick(pick) => {
                assert_eq!(pick.subchannel.address().address.to_string(), "addr1");
                pick.subchannel
            }
            other => panic!("unexpected pick result {other:?}"),
        };

        // 2. Simulate addr1 disconnecting (transitioning to Idle).
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

        env.expect_no_events();

        // 4. Simulate an RPC happening.
        let pick_result = idle_picker.pick(&RequestHeaders::default());
        assert!(matches!(pick_result, PickResult::Queue));
        let data = env.expect_schedule_work();
        assert!(data.is_none());

        // 5. Call work to execute the scheduled connection attempt.
        env.policy.work(data, &mut env.tcc);

        // Reconnect creates subchannels for both addr1 and addr2.
        env.expect_new_subchannel();
        env.expect_new_subchannel();

        // 6. Verify that the policy initiates reconnection to addr1.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);

        // 7. Simulate addr1 failing on reconnect.
        let sc1_reconnect = env.policy.subchannels()[0].clone();
        env.send_subchannel_update(
            &sc1_reconnect,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("connection refused".to_string()),
            },
        );

        // 8. Policy should failover to addr2.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");
    }

    // Tests that when a resolver update arrives while in FirstPass, any subchannels
    // that have already failed (TransientFailure) preserve their state so the policy
    // does not redundantly re-attempt them before trying remaining addresses.
    #[tokio::test]
    async fn test_pick_first_resolver_update_preserves_subchannel_failure_state() {
        let mut env = new_env();

        // 1. Initial endpoints [addr1, addr2].
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Idle);

        env.policy.exit_idle(&mut env.tcc);

        let sc1 = env.expect_new_subchannel();
        let _sc2 = env.expect_new_subchannel();
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);

        // 2. addr1 fails with TransientFailure. Frontier advances to addr2.
        env.send_subchannel_update(
            &sc1,
            &SubchannelState {
                connectivity_state: ConnectivityState::TransientFailure,
                last_connection_error: Some("connection refused".to_string()),
            },
        );
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr2");

        // 3. New resolver update [addr1, addr3]. (Retains addr1 which already failed).
        let endpoints_new = create_endpoints(vec!["addr1", "addr3"], None);
        env.send_resolver_update(endpoints_new).unwrap();

        // 4. addr3 is created as a new subchannel.
        let _sc3 = env.expect_new_subchannel();

        // 5. Because addr1 is already in TransientFailure, the policy should skip addr1
        // and immediately connect addr3.
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr3");
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
    }

    // Tests that when in IDLE, receiving resolver updates leaves the policy in IDLE
    // without creating subchannels or connecting until exit_idle is called.
    #[tokio::test]
    async fn test_pick_first_idle_on_resolver_update_until_exit_idle() {
        let mut env = new_env();

        // 1. Send initial resolver update [addr1, addr2].
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();

        // 2. Expect IDLE picker update and NO subchannels or connect attempts.
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Idle);
        env.expect_no_events();

        // 3. Send second resolver update [addr3, addr4] while still in IDLE.
        let endpoints2 = create_endpoints(vec!["addr3", "addr4"], None);
        env.send_resolver_update(endpoints2).unwrap();

        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Idle);
        env.expect_no_events();

        // 4. Explicitly exit idle.
        env.policy.exit_idle(&mut env.tcc);

        // 5. Verify that subchannels are now created for the latest address list [addr3,
        //    addr4]
        // and connection begins.
        env.expect_new_subchannel();
        env.expect_new_subchannel();
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr3");
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
    }

    // Tests that when in Transient Failure, receiving a valid resolver update
    // immediately transitions to connecting without waiting for exit_idle.
    #[tokio::test]
    async fn test_pick_first_resolver_update_in_transient_failure_starts_connecting_immediately() {
        let mut env = new_env();

        // 1. Initial resolution failure sets Transient Failure.
        let res = env.send_resolver_error("resolver unavailable".to_string());
        assert!(res.is_err());

        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();

        // 2. A valid resolver update arrives [addr1, addr2].
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();

        // 3. Verify subchannels created and connection initiates immediately without
        //    exit_idle.
        env.expect_new_subchannel();
        env.expect_new_subchannel();
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
    }

    // Tests that calling exit_idle on an empty IdleState transitions to Transient
    // Failure, requests resolution, and immediately starts connecting once valid
    // endpoints arrive.
    #[tokio::test]
    async fn test_pick_first_exit_idle_with_empty_addresses_transitions_to_transient_failure_and_recovers()
     {
        let mut env = new_env();

        // 1. Calling exit_idle before any resolver update.
        env.policy.exit_idle(&mut env.tcc);

        // 2. Expect TransientFailure and resolution request.
        let state = env.expect_picker_update();
        assert_eq!(
            state.connectivity_state,
            ConnectivityState::TransientFailure
        );
        env.expect_request_resolution();

        // 3. Name resolution completes with [addr1, addr2].
        let endpoints = create_endpoints(vec!["addr1", "addr2"], None);
        env.send_resolver_update(endpoints).unwrap();

        // 4. Verify that subchannels are created and connection initiates immediately.
        env.expect_new_subchannel();
        env.expect_new_subchannel();
        let addr = env.expect_connect();
        assert_eq!(addr.address.to_string(), "addr1");
        let state = env.expect_picker_update();
        assert_eq!(state.connectivity_state, ConnectivityState::Connecting);
    }
}
