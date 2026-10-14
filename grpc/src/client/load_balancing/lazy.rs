/*
 *
 * Copyright 2026 gRPC authors.
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

use std::mem::replace;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::client::ConnectivityState;
use crate::client::RequestHeaders;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::name_resolution::ResolverUpdate;

/// Implements a "lazy" [`LbPolicy`].  Normally LB policies begin in a
/// [`ConnectivityState::Connecting`] state, but [`Lazy`] waits for the first
/// picker call or explicit [`LbPolicy::exit_idle`] call before constructing the
/// delegate LB policy or calling its [`LbPolicy::resolver_update`] method.
/// Note that Lazy can only properly wrap a policy whose config is Clone, as it
/// needs to store the config until the child is built.
#[derive(Debug)]
pub struct Lazy<T: LbPolicyBuilder> {
    inner: Inner<T>,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum Inner<T: LbPolicyBuilder> {
    Void,
    Pending(Pending<T>),
    Built(T::LbPolicy),
}

#[derive(Debug)]
struct Pending<T: LbPolicyBuilder> {
    delegate_builder: T,
    options: LbPolicyOptions,
    latest_state: Option<(ResolverUpdate, <T::LbPolicy as LbPolicy>::LbConfig)>,
}

impl<T: LbPolicyBuilder> Lazy<T> {
    /// Creates a wrapper for `T`.  An idle picker that will wake it up lazily
    /// is produced upon the first resolver update.
    pub fn new(delegate_builder: T, options: LbPolicyOptions) -> Self {
        Self {
            inner: Inner::Pending(Pending {
                delegate_builder,
                options,
                latest_state: None,
            }),
        }
    }
}

impl<T: LbPolicyBuilder> LbPolicy for Lazy<T>
where
    <T::LbPolicy as LbPolicy>::LbConfig: Clone,
{
    type LbConfig = <T::LbPolicy as LbPolicy>::LbConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: &Self::LbConfig,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        match &mut self.inner {
            Inner::Void => unreachable!(),
            Inner::Pending(pending) => {
                if pending.latest_state.is_none() {
                    // This is the first update; produce the idle picker.
                    channel_controller.update_picker(LbState {
                        connectivity_state: ConnectivityState::Idle,
                        picker: Arc::new(WakeUpPicker::new(pending.options.work_scheduler.clone())),
                    });
                }
                pending.latest_state = Some((update, config.clone()));
                Ok(())
            }
            Inner::Built(delegate) => delegate.resolver_update(update, config, channel_controller),
        }
    }

    fn work(&mut self, data: Option<WorkData>, channel_controller: &mut dyn ChannelController) {
        if let Inner::Built(delegate) = &mut self.inner {
            delegate.work(data, channel_controller);
        } else {
            // The channel should only give us a work call if we asked for it
            // via the WakeUpPicker.
            self.exit_idle(channel_controller);
        }
    }

    fn exit_idle(&mut self, channel_controller: &mut dyn ChannelController) {
        if let Inner::Built(delegate) = &mut self.inner {
            delegate.exit_idle(channel_controller);
            return;
        }

        let Inner::Pending(Pending {
            delegate_builder,
            options,
            latest_state,
        }) = replace(&mut self.inner, Inner::Void)
        else {
            unreachable!();
        };

        let mut delegate = delegate_builder.build(options);
        // If there is a pending update, send it now.  Otherwise just exit_idle.
        if let Some((update, config)) = latest_state {
            if delegate
                .resolver_update(update, &config, channel_controller)
                .is_err()
            {
                // Notify the channel that it should try to retrieve a new update.
                // TODO: log the error so it isn't completely lost.
                channel_controller.request_resolution();
            }
        } else {
            delegate.exit_idle(channel_controller);
        }
        self.inner = Inner::Built(delegate);
    }
}

/// Implements a [`Picker`] that schedules work for the current policy (intended
/// to wake up the wrapped delegate policy) and queues every RPC.
#[derive(Debug)]
pub struct WakeUpPicker {
    work_scheduler: Arc<dyn WorkScheduler>,
    triggered_work: AtomicBool,
}

impl WakeUpPicker {
    fn new(work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        Self {
            work_scheduler,
            triggered_work: AtomicBool::new(false),
        }
    }
}

impl Picker for WakeUpPicker {
    fn pick(&self, request: &RequestHeaders) -> PickResult {
        if !self.triggered_work.swap(true, Ordering::Relaxed) {
            self.work_scheduler.schedule_work(None);
        }
        PickResult::Queue
    }
}
#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::client::load_balancing::test_utils::TestEnv;
    use crate::client::load_balancing::test_utils::new_request_headers;
    use crate::rt::default_runtime;

    #[derive(Debug, PartialEq, Eq)]
    enum MockEvent {
        Build,
        ResolverUpdate,
        Work,
        ExitIdle,
    }

    // Constructs the test environment for a Lazy policy wrapping a MockPolicy.
    // Also returns the receiver for the MockPolicy's events.
    fn new_env() -> (TestEnv<Lazy<MockPolicy>>, mpsc::Receiver<MockEvent>) {
        let (builder, rx) = MockPolicy::new();
        let env = TestEnv::new(|work_scheduler| {
            let options = LbPolicyOptions {
                work_scheduler,
                runtime: default_runtime(),
            };
            Lazy::new(builder, options)
        });
        (env, rx)
    }

    // Tests that the delegate policy is constructed only after exit_idle is
    // called and latches the previous resolver update.
    #[test]
    fn test_lazy_build_on_exit_idle() {
        let (mut env, rx) = new_env();

        // No picker is produced until the first update.
        env.expect_no_events();

        // Give lazy an update.
        env.send_resolver_update(vec![]).unwrap();

        // Verify that the initial picker is Idle.
        let lb_state = env.expect_picker_update();
        assert_eq!(lb_state.connectivity_state, ConnectivityState::Idle);

        // Ensure delegate is not built yet.
        assert!(rx.try_recv().is_err());

        // Call exit_idle.
        env.policy.exit_idle(&mut env.tcc);

        // Verify delegate was built.
        assert_eq!(rx.recv().unwrap(), MockEvent::Build);
        // Verify delegate received the cached update.
        assert_eq!(rx.recv().unwrap(), MockEvent::ResolverUpdate);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    // Tests that the delegate policy is constructed only after the picker is
    // called and latches the previous resolver update.
    #[test]
    fn test_lazy_build_on_pick() {
        let (mut env, rx) = new_env();

        // Give lazy an update and get the resulting picker.
        env.send_resolver_update(vec![]).unwrap();
        let lb_state = env.expect_picker_update();

        // Call pick on the picker.
        let res = lb_state.picker.pick(&new_request_headers());

        // PickResult should be Queue.
        assert!(matches!(res, PickResult::Queue));

        // Picking should have scheduled work.
        let data = env.expect_schedule_work();
        assert!(data.is_none());

        // Call work on lazy to honor its request.
        env.policy.work(data, &mut env.tcc);

        // Verify delegate was built and received the pending update.
        assert_eq!(rx.recv().unwrap(), MockEvent::Build);
        assert_eq!(rx.recv().unwrap(), MockEvent::ResolverUpdate);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    // Tests that calling pick multiple times on the WakeUpPicker only schedules
    // a single work event.
    #[test]
    fn test_lazy_pick_squashes_work_calls() {
        let (mut env, _rx) = new_env();

        // Give lazy an update and get the resulting picker.
        env.send_resolver_update(vec![]).unwrap();
        let lb_state = env.expect_picker_update();

        // Call pick multiple times.
        for _ in 0..10 {
            let res = lb_state.picker.pick(&new_request_headers());
            assert!(matches!(res, PickResult::Queue));
        }

        // Only a single work item should have been scheduled.
        assert!(env.expect_schedule_work().is_none());
        env.expect_no_events();
    }

    // Tests that the delegate policy is constructed only after exit_idle is
    // called even when there is no pending resolver update.
    #[test]
    fn test_lazy_exit_idle_without_update() {
        let (mut env, rx) = new_env();

        // Call exit_idle without update
        env.policy.exit_idle(&mut env.tcc);

        // Verify delegate was built and received the exit_idle call.
        assert_eq!(rx.recv().unwrap(), MockEvent::Build);
        assert_eq!(rx.recv().unwrap(), MockEvent::ExitIdle);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    // Tests that only the first resolver update produces a picker, and that
    // only the latest update is delivered once the delegate is built.
    #[test]
    fn test_lazy_multiple_updates_before_build() {
        let (mut env, rx) = new_env();

        // Only the first update produces a picker.
        env.send_resolver_update(vec![]).unwrap();
        env.expect_picker_update();
        env.send_resolver_update(vec![]).unwrap();
        env.expect_no_events();

        // Ensure delegate is not built yet.
        assert!(rx.try_recv().is_err());

        // Call exit_idle.
        env.policy.exit_idle(&mut env.tcc);

        // Verify delegate was built and received a single update.
        assert_eq!(rx.recv().unwrap(), MockEvent::Build);
        assert_eq!(rx.recv().unwrap(), MockEvent::ResolverUpdate);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    /// Implements both LbPolicyBuilder and LbPolicy to send events on a
    /// channel.
    #[derive(Debug, Clone)]
    struct MockPolicy {
        tx: mpsc::Sender<MockEvent>,
    }

    impl MockPolicy {
        fn new() -> (Self, mpsc::Receiver<MockEvent>) {
            let (tx, rx) = mpsc::channel();
            (Self { tx }, rx)
        }
    }

    impl LbPolicyBuilder for MockPolicy {
        type LbPolicy = Self;

        fn build(&self, _options: LbPolicyOptions) -> Self {
            self.tx.send(MockEvent::Build).unwrap();
            self.clone()
        }
        fn name(&self) -> &'static str {
            "mock"
        }
        fn parse_config(
            &self,
            _config: &crate::client::load_balancing::LbConfigJson,
        ) -> Result<<Self::LbPolicy as LbPolicy>::LbConfig, String> {
            Ok(())
        }
    }

    impl LbPolicy for MockPolicy {
        type LbConfig = ();

        fn resolver_update(
            &mut self,
            _update: ResolverUpdate,
            _config: &(),
            _channel_controller: &mut dyn ChannelController,
        ) -> Result<(), String> {
            self.tx.send(MockEvent::ResolverUpdate).unwrap();
            Ok(())
        }
        fn work(
            &mut self,
            _work_data: Option<WorkData>,
            _channel_controller: &mut dyn ChannelController,
        ) {
            self.tx.send(MockEvent::Work).unwrap();
        }
        fn exit_idle(&mut self, _channel_controller: &mut dyn ChannelController) {
            self.tx.send(MockEvent::ExitIdle).unwrap();
        }
    }
}
