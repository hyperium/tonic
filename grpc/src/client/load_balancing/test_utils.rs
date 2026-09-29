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

use std::any::Any;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::mpsc;

use serde::Deserialize;
use serde::Serialize;

use crate::client::RequestHeaders;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::DynLbConfig;
use crate::client::load_balancing::DynLbPolicy;
use crate::client::load_balancing::LbConfigJson;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::Subchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::subchannel::ForwardingSubchannel;
use crate::client::load_balancing::subchannel::SubchannelUpdate;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::Address;

pub(crate) fn new_request_headers() -> RequestHeaders {
    RequestHeaders::default()
}

// A test subchannel that forwards connect calls to a channel.
// This allows tests to verify when a subchannel is asked to connect.
pub(crate) struct TestSubchannel {
    address: Address,
    tx_connect: mpsc::Sender<TestEvent>,
    // The work scheduler provided by the policy that created this subchannel,
    // used to deliver state updates about it.  None for subchannels that were
    // not created through a TestChannelController.
    work_scheduler: Option<Arc<dyn WorkScheduler>>,
}

impl TestSubchannel {
    pub fn new(address: Address, tx_connect: mpsc::Sender<TestEvent>) -> Self {
        Self {
            address,
            tx_connect,
            work_scheduler: None,
        }
    }

    pub fn new_with_work_scheduler(
        address: Address,
        tx_connect: mpsc::Sender<TestEvent>,
        work_scheduler: Arc<dyn WorkScheduler>,
    ) -> Self {
        Self {
            address,
            tx_connect,
            work_scheduler: Some(work_scheduler),
        }
    }
}

/// Simulates a state change of `subchannel` by scheduling the update on the
/// WorkScheduler that the creating policy passed to `new_subchannel`, exactly
/// as the channel would.
///
/// `subchannel` must have been created by a [`TestChannelController`].  The
/// resulting work is observable via [`TestEnv::expect_schedule_work`], and
/// the data it contains should be passed to the policy's `work` method.
pub(crate) fn schedule_subchannel_update(subchannel: &Arc<dyn Subchannel>, state: SubchannelState) {
    let sc = subchannel
        .downcast_ref::<TestSubchannel>()
        .expect("subchannel was not created by a TestChannelController");
    let work_scheduler = sc
        .work_scheduler
        .as_ref()
        .expect("subchannel has no work scheduler");
    work_scheduler.schedule_work(Some(Box::new(SubchannelUpdate::new(
        subchannel.clone(),
        state,
    ))));
}

impl ForwardingSubchannel for TestSubchannel {
    fn delegate(&self) -> &Arc<dyn Subchannel> {
        panic!("unsupported operation on a test subchannel");
    }

    fn address(&self) -> Address {
        self.address.clone()
    }

    fn connect(&self) {
        println!("connect called for subchannel {}", self.address);
        self.tx_connect
            .send(TestEvent::Connect(self.address.clone()))
            .unwrap();
    }
}

impl Hash for TestSubchannel {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.address.hash(state);
    }
}

impl PartialEq for TestSubchannel {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Eq for TestSubchannel {}

/// Events created by LB policies by calling the channel controller.
pub(crate) enum TestEvent {
    NewSubchannel(Arc<dyn Subchannel>),
    UpdatePicker(LbState),
    RequestResolution,
    Connect(Address),
}

// TODO(easwars): Remove this and instead derive Debug.
impl Debug for TestEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NewSubchannel(sc) => write!(f, "NewSubchannel({})", sc.address()),
            Self::UpdatePicker(state) => write!(f, "UpdatePicker({})", state.connectivity_state),
            Self::RequestResolution => write!(f, "RequestResolution"),
            Self::Connect(addr) => write!(f, "Connect({:?})", addr.address),
        }
    }
}

/// A test channel controller that forwards calls to a channel.  This allows
/// tests to verify when a channel controller is asked to create subchannels or
/// update the picker.
pub(crate) struct TestChannelController {
    pub(crate) tx_events: mpsc::Sender<TestEvent>,
}

impl ChannelController for TestChannelController {
    fn new_subchannel(
        &mut self,
        address: &Address,
        work_scheduler: Arc<dyn WorkScheduler>,
    ) -> (Arc<dyn Subchannel>, SubchannelState) {
        println!("new_subchannel called for address {}", address);
        let subchannel: Arc<dyn Subchannel> = Arc::new(TestSubchannel::new_with_work_scheduler(
            address.clone(),
            self.tx_events.clone(),
            work_scheduler,
        ));
        self.tx_events
            .send(TestEvent::NewSubchannel(subchannel.clone()))
            .unwrap();
        (subchannel, SubchannelState::idle())
    }
    fn update_picker(&mut self, update: LbState) {
        println!("picker_update called with {}", update.connectivity_state);
        self.tx_events
            .send(TestEvent::UpdatePicker(update))
            .unwrap();
    }
    fn request_resolution(&mut self) {
        self.tx_events.send(TestEvent::RequestResolution).unwrap();
    }
}

#[derive(Debug)]
pub(crate) struct TestWorkScheduler {
    pub(crate) tx_work: mpsc::Sender<Option<WorkData>>,
}

impl WorkScheduler for TestWorkScheduler {
    fn schedule_work(&self, data: Option<WorkData>) {
        // Subchannels schedule work when they are dropped, which can happen
        // after the test has stopped listening.  Ignore the error rather than
        // panicking inside a Drop impl.
        let _ = self.tx_work.send(data);
    }
}

/// Delivers all work currently scheduled on `rx_work` to `policy`, along with
/// any work that results from it, until no work is left.
///
/// Prefer [`TestEnv::run_work`].  This is for tests that use a channel
/// controller other than [`TestChannelController`] and so cannot use
/// [`TestEnv`].
pub(crate) fn run_pending_work<P: LbPolicy + ?Sized>(
    policy: &mut P,
    rx_work: &mpsc::Receiver<Option<WorkData>>,
    channel_controller: &mut dyn ChannelController,
) {
    let mut got_work = false;
    while let Ok(data) = rx_work.try_recv() {
        got_work = true;
        policy.work(data, channel_controller);
    }
    assert!(got_work, "no work received when asked to run pending work");
}

/// A test environment for an LB policy of type `P`.
///
/// `expect_*` methods consume events from the appropriate source.  Methods to
/// actually control the policy are available if `P` implements [`LbPolicy`].
/// Others can still use this type for assertions, but call the policy manually.
pub(crate) struct TestEnv<P> {
    pub(crate) policy: P,
    pub(crate) tcc: TestChannelController,
    pub(crate) rx_events: mpsc::Receiver<TestEvent>,
    pub(crate) rx_work: mpsc::Receiver<Option<WorkData>>,
}

impl<P> TestEnv<P> {
    /// Creates the test environment using a LbPolicyBuilder-like closure for
    /// policies that don't implement LbPolicyBuilder.
    pub(crate) fn new(build_policy: impl FnOnce(Arc<dyn WorkScheduler>) -> P) -> Self {
        let (tx_events, rx_events) = mpsc::channel::<TestEvent>();
        let (tx_work, rx_work) = mpsc::channel::<Option<WorkData>>();
        let work_scheduler = Arc::new(TestWorkScheduler { tx_work });
        Self {
            policy: build_policy(work_scheduler),
            tcc: TestChannelController { tx_events },
            rx_events,
            rx_work,
        }
    }

    // Returns the next event, or panics if there is none.
    fn next_event(&mut self) -> TestEvent {
        match self.rx_events.try_recv() {
            Ok(event) => event,
            Err(e) => panic!("expected event, got error: {e:?}"),
        }
    }

    /// Verifies that the policy created a subchannel, and returns it.
    pub(crate) fn expect_new_subchannel(&mut self) -> Arc<dyn Subchannel> {
        match self.next_event() {
            TestEvent::NewSubchannel(sc) => sc,
            other => panic!("expected NewSubchannel event, got {other:?}"),
        }
    }

    /// Verifies that the policy asked a subchannel to connect, and returns that
    /// subchannel's address.
    pub(crate) fn expect_connect(&mut self) -> Address {
        match self.next_event() {
            TestEvent::Connect(addr) => addr,
            other => panic!("expected Connect event, got {other:?}"),
        }
    }

    /// Verifies that the policy produced a new picker, and returns the state
    /// containing it.
    pub(crate) fn expect_picker_update(&mut self) -> LbState {
        match self.next_event() {
            TestEvent::UpdatePicker(state) => state,
            other => panic!("expected UpdatePicker event, got {other:?}"),
        }
    }

    /// Verifies that the policy requested re-resolution.
    pub(crate) fn expect_request_resolution(&mut self) {
        match self.next_event() {
            TestEvent::RequestResolution => {}
            other => panic!("expected RequestResolution event, got {other:?}"),
        }
    }

    /// Verifies that the policy scheduled work, and returns the data it was
    /// scheduled with.
    pub(crate) fn expect_schedule_work(&mut self) -> Option<WorkData> {
        match self.rx_work.try_recv() {
            Ok(data) => data,
            Err(e) => panic!("expected scheduled work, got error: {e:?}"),
        }
    }

    /// Verifies that the policy has produced no further events and has not
    /// scheduled any further work.
    pub(crate) fn expect_no_events(&mut self) {
        if let Ok(event) = self.rx_events.try_recv() {
            panic!("expected no events, got {event:?}");
        }
        if let Ok(data) = self.rx_work.try_recv() {
            panic!("expected no events, got scheduled work {data:?}");
        }
    }
}

impl<P: LbPolicy> TestEnv<P> {
    /// Sends a resolver update containing `endpoints` to the policy, using the
    /// policy's default config.
    pub(crate) fn send_resolver_update(&mut self, endpoints: Vec<Endpoint>) -> Result<(), String>
    where
        P::LbConfig: Default,
    {
        let update = ResolverUpdate {
            endpoints: Ok(endpoints),
            ..Default::default()
        };
        self.policy
            .resolver_update(update, &P::LbConfig::default(), &mut self.tcc)
    }

    /// Sends a resolver error to the policy, using the policy's default config.
    pub(crate) fn send_resolver_error(&mut self, err: String) -> Result<(), String>
    where
        P::LbConfig: Default,
    {
        let update = ResolverUpdate {
            endpoints: Err(err),
            ..Default::default()
        };
        self.policy
            .resolver_update(update, &P::LbConfig::default(), &mut self.tcc)
    }

    /// Simulates a state change of `subchannel` and delivers it to the policy
    /// via the work scheduler.  Any follow-up work is not run; use
    /// [`TestEnv::run_work`] if this can trigger additional work items.
    pub(crate) fn send_subchannel_update(
        &mut self,
        subchannel: &Arc<dyn Subchannel>,
        state: &SubchannelState,
    ) {
        self.expect_no_events();
        schedule_subchannel_update(subchannel, state.clone());
        let data = self.expect_schedule_work();
        self.policy.work(data, &mut self.tcc);
    }

    /// Delivers all work currently scheduled to the policy, and any work that
    /// results from it, until none is left.
    pub(crate) fn run_work(&mut self) {
        run_pending_work(&mut self.policy, &self.rx_work, &mut self.tcc);
    }
}

// The callback to invoke when resolver_update is invoked on the stub policy.
type ResolverUpdateFn = Arc<
    dyn Fn(
            &mut StubPolicyData,
            ResolverUpdate,
            &DynLbConfig,
            &mut dyn ChannelController,
        ) -> Result<(), String>
        + Send
        + Sync,
>;

type ExitIdleFn = Arc<dyn Fn(&mut StubPolicyData, &mut dyn ChannelController) + Send + Sync>;

type WorkFn =
    Arc<dyn Fn(&mut StubPolicyData, Option<WorkData>, &mut dyn ChannelController) + Send + Sync>;

/// This struct holds `LbPolicy` trait stub functions that tests are expected to
/// implement.
#[derive(Clone, Default)]
pub(crate) struct StubPolicyFuncs {
    pub resolver_update: Option<ResolverUpdateFn>,
    pub exit_idle: Option<ExitIdleFn>,
    pub work: Option<WorkFn>,
}

impl Debug for StubPolicyFuncs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stub funcs")
    }
}

/// Data holds test data that will be passed all to functions in PolicyFuncs
#[derive(Debug)]
pub(crate) struct StubPolicyData {
    pub lb_policy_options: LbPolicyOptions,
    pub test_data: Option<Box<dyn Any + Send + Sync>>,
}

impl StubPolicyData {
    /// Creates an instance of StubPolicyData.
    pub fn new(lb_policy_options: LbPolicyOptions) -> Self {
        Self {
            test_data: None,
            lb_policy_options,
        }
    }
}

/// The stub `LbPolicy` that calls the provided functions.
#[derive(Debug)]
pub(crate) struct StubPolicy {
    funcs: StubPolicyFuncs,
    data: StubPolicyData,
}

impl LbPolicy for StubPolicy {
    type LbConfig = DynLbConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &DynLbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        if let Some(f) = &mut self.funcs.resolver_update {
            return f(&mut self.data, update, config, channel_controller);
        }
        Ok(())
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        if let Some(f) = &self.funcs.exit_idle {
            f(&mut self.data, channel_controller);
        }
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        if let Some(f) = &self.funcs.work {
            f(&mut self.data, data, channel_controller);
        }
    }
}

impl StubPolicy {
    pub(crate) fn new(funcs: StubPolicyFuncs, options: LbPolicyOptions) -> Self {
        Self {
            funcs,
            data: StubPolicyData::new(options),
        }
    }
}

/// StubPolicyBuilder builds a StubLbPolicy.
#[derive(Debug)]
pub(crate) struct StubPolicyBuilder {
    name: &'static str,
    funcs: StubPolicyFuncs,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(super) struct MockConfig {
    shuffle_address_list: Option<bool>,
}

impl LbPolicyBuilder for StubPolicyBuilder {
    type LbPolicy = Box<DynLbPolicy>;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        let data = StubPolicyData::new(options);
        Box::new(StubPolicy {
            funcs: self.funcs.clone(),
            data,
        })
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn parse_config(&self, config: &LbConfigJson) -> Result<DynLbConfig, String> {
        let cfg: MockConfig = match config.convert_to() {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("failed to parse JSON config: {}", e));
            }
        };
        Ok(Arc::new(cfg))
    }
}

pub(crate) fn reg_stub_policy(name: &'static str, funcs: StubPolicyFuncs) {
    super::GLOBAL_LB_REGISTRY.add_dyn_builder(Arc::new(StubPolicyBuilder { name, funcs }));
}
