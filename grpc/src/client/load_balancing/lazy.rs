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

use crate::client::ConnectivityState;
use crate::client::load_balancing::ChannelController;
use crate::client::load_balancing::LbPolicy;
use crate::client::load_balancing::LbPolicyBuilder;
use crate::client::load_balancing::LbPolicyOptions;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::Subchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::load_balancing::WorkScheduler;
use crate::client::name_resolution::ResolverUpdate;
use crate::core::RequestHeaders;

/// Implements a "lazy" [`LbPolicy`].  Normally LB policies begin in a
/// [`ConnectivityState::Connecting`] state, but [`Lazy`] waits for the first
/// picker call or explicit [`LbPolicy::exit_idle`] call before constructing the
/// delegate LB policy or calling its [`LbPolicy::resolver_update`] method.
/// Note that Lazy can only properly wrap a policy whose config is Clone, as it
/// needs to store the config until the child is built.
#[derive(Debug)]
pub(crate) struct Lazy<T: LbPolicyBuilder> {
    inner: Inner<T>,
}

#[derive(Debug)]
enum Inner<T: LbPolicyBuilder> {
    Void,
    Pending(Pending<T>),
    Built(T::LbPolicy),
}

#[derive(Debug)]
struct Pending<T: LbPolicyBuilder> {
    delegate_builder: T,
    options: LbPolicyOptions,
    latest_state: Option<(ResolverUpdate, Option<<T::LbPolicy as LbPolicy>::LbConfig>)>,
}

impl<T: LbPolicyBuilder> Lazy<T> {
    /// Creates a wrapper for `T` and immediately produces an idle picker that
    /// will wake it up lazily.
    pub fn new(
        delegate_builder: T,
        options: LbPolicyOptions,
        channel_controller: &mut dyn ChannelController,
    ) -> Self {
        channel_controller.update_picker(LbState {
            connectivity_state: ConnectivityState::Idle,
            picker: Arc::new(WakeUpPicker::new(options.work_scheduler.clone())),
        });
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
        config: Option<&Self::LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        match &mut self.inner {
            Inner::Void => unreachable!(),
            Inner::Pending(pending) => {
                pending.latest_state = Some((update, config.cloned()));
                Ok(())
            }
            Inner::Built(delegate) => delegate.resolver_update(update, config, channel_controller),
        }
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        if let Inner::Built(delegate) = &mut self.inner {
            delegate.subchannel_update(subchannel, state, channel_controller);
        }
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        if let Inner::Built(delegate) = &mut self.inner {
            delegate.work(channel_controller);
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
                .resolver_update(update, config.as_ref(), channel_controller)
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
}

impl WakeUpPicker {
    fn new(work_scheduler: Arc<dyn WorkScheduler>) -> Self {
        Self { work_scheduler }
    }
}

impl Picker for WakeUpPicker {
    fn pick(&self, request: &RequestHeaders) -> PickResult {
        self.work_scheduler.schedule_work();
        PickResult::Queue
    }
}
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::*;
    use crate::client::load_balancing::test_utils::TestChannelController;
    use crate::client::load_balancing::test_utils::TestEvent;
    use crate::client::load_balancing::test_utils::TestWorkScheduler;
    use crate::client::load_balancing::test_utils::new_request_headers;

    #[derive(Debug, PartialEq, Eq)]
    enum MockEvent {
        Build,
        ResolverUpdate,
        SubchannelUpdate,
        Work,
        ExitIdle,
    }

    // Tests that the delegate policy is constructed only after exit_idle is
    // called and latches the previous resolver update.
    #[tokio::test]
    async fn test_lazy_build_on_exit_idle() {
        let (builder, mut rx) = MockPolicy::new();

        let (tx_events, mut rx_events) = mpsc::unbounded_channel();
        let mut cc = TestChannelController {
            tx_events: tx_events.clone(),
        };
        let options = LbPolicyOptions {
            work_scheduler: Arc::new(TestWorkScheduler { tx_events }),
            runtime: crate::rt::default_runtime(),
        };

        let mut lazy = Lazy::new(builder, options, &mut cc);

        // Verify that the initial picker is WakeUpPicker and state is Idle.
        let event = rx_events.recv().await.unwrap();
        let TestEvent::UpdatePicker(lb_state) = event else {
            panic!("expected UpdatePicker event");
        };
        assert_eq!(lb_state.connectivity_state, ConnectivityState::Idle);

        // Give lazy an update.
        lazy.resolver_update(ResolverUpdate::default(), None, &mut cc)
            .unwrap();

        // Ensure delegate is not built yet.
        assert!(rx.try_recv().is_err());

        // Call exit_idle.
        lazy.exit_idle(&mut cc);

        // Verify delegate was built.
        assert_eq!(rx.recv().await.unwrap(), MockEvent::Build);
        // Verify delegate received the cached update.
        assert_eq!(rx.recv().await.unwrap(), MockEvent::ResolverUpdate);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    // Tests that the delegate policy is constructed only after the picker is
    // called and latches the previous resolver update.
    #[tokio::test]
    async fn test_lazy_build_on_pick() {
        let (builder, mut rx) = MockPolicy::new();

        let (tx_events, mut rx_events) = mpsc::unbounded_channel();
        let mut cc = TestChannelController {
            tx_events: tx_events.clone(),
        };
        let options = LbPolicyOptions {
            work_scheduler: Arc::new(TestWorkScheduler { tx_events }),
            runtime: crate::rt::default_runtime(),
        };

        let mut lazy = Lazy::new(builder, options, &mut cc);

        // Get the initial picker so we can send it a pick.
        let event = rx_events.recv().await.unwrap();
        let TestEvent::UpdatePicker(lb_state) = event else {
            panic!("expected UpdatePicker event");
        };

        // Give lazy an update.
        lazy.resolver_update(ResolverUpdate::default(), None, &mut cc)
            .unwrap();

        // Call pick on the picker.
        let res = lb_state.picker.pick(&new_request_headers());

        // PickResult should be Queue.
        assert!(matches!(res, PickResult::Queue));

        // Picking should have scheduled work.
        let event = rx_events.recv().await.unwrap();
        assert!(matches!(event, TestEvent::ScheduleWork));

        // Call work on lazy to honor its request.
        lazy.work(&mut cc);

        // Verify delegate was built and received the pending update.
        assert_eq!(rx.recv().await.unwrap(), MockEvent::Build);
        assert_eq!(rx.recv().await.unwrap(), MockEvent::ResolverUpdate);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    // Tests that the delegate policy is constructed only after exit_idle is
    // called even when there is no pending resolver update.
    #[tokio::test]
    async fn test_lazy_exit_idle_without_update() {
        let (builder, mut rx) = MockPolicy::new();

        let (tx_events, mut rx_events) = mpsc::unbounded_channel();
        let mut cc = TestChannelController {
            tx_events: tx_events.clone(),
        };
        let options = LbPolicyOptions {
            work_scheduler: Arc::new(TestWorkScheduler { tx_events }),
            runtime: crate::rt::default_runtime(),
        };

        let mut lazy = Lazy::new(builder, options, &mut cc);

        // Lazy always produces an UpdatePicker immediately.
        assert!(matches!(
            rx_events.recv().await.unwrap(),
            TestEvent::UpdatePicker(_)
        ));

        // Call exit_idle without update
        lazy.exit_idle(&mut cc);

        // Verify delegate was built and received the exit_idle call.
        assert_eq!(rx.recv().await.unwrap(), MockEvent::Build);
        assert_eq!(rx.recv().await.unwrap(), MockEvent::ExitIdle);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    // Tests that the delegate policy is constructed only after the picker is
    // called and sees exit_idle, when there is no pending resolver update.
    #[tokio::test]
    async fn test_lazy_build_on_pick_without_update() {
        let (builder, mut rx) = MockPolicy::new();

        let (tx_events, mut rx_events) = mpsc::unbounded_channel();
        let mut cc = TestChannelController {
            tx_events: tx_events.clone(),
        };
        let options = LbPolicyOptions {
            work_scheduler: Arc::new(TestWorkScheduler { tx_events }),
            runtime: crate::rt::default_runtime(),
        };

        let mut lazy = Lazy::new(builder, options, &mut cc);

        // Get the initial picker so we can send it a pick.
        let event = rx_events.recv().await.unwrap();
        let TestEvent::UpdatePicker(lb_state) = event else {
            panic!("expected UpdatePicker event");
        };

        // Call pick on the picker.
        let res = lb_state.picker.pick(&new_request_headers());

        // PickResult should be Queue.
        assert!(matches!(res, PickResult::Queue));

        // Picking should have scheduled work.
        let event = rx_events.recv().await.unwrap();
        assert!(matches!(event, TestEvent::ScheduleWork));

        // Call work on lazy to honor its request.
        lazy.work(&mut cc);

        // Verify delegate was built and received an exit_idle call.
        assert_eq!(rx.recv().await.unwrap(), MockEvent::Build);
        assert_eq!(rx.recv().await.unwrap(), MockEvent::ExitIdle);
        // Verify no more events.
        assert!(rx.try_recv().is_err());
    }

    /// Implements both LbPolicyBuilder and LbPolicy to send events on a
    /// channel.
    #[derive(Debug, Clone)]
    struct MockPolicy {
        tx: mpsc::UnboundedSender<MockEvent>,
    }

    impl MockPolicy {
        fn new() -> (Self, mpsc::UnboundedReceiver<MockEvent>) {
            let (tx, rx) = mpsc::unbounded_channel();
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
    }

    impl LbPolicy for MockPolicy {
        type LbConfig = ();

        fn resolver_update(
            &mut self,
            _update: ResolverUpdate,
            _config: Option<&()>,
            _channel_controller: &mut dyn ChannelController,
        ) -> Result<(), String> {
            self.tx.send(MockEvent::ResolverUpdate).unwrap();
            Ok(())
        }
        fn subchannel_update(
            &mut self,
            _subchannel: Arc<dyn Subchannel>,
            _state: &SubchannelState,
            _channel_controller: &mut dyn ChannelController,
        ) {
            self.tx.send(MockEvent::SubchannelUpdate).unwrap();
        }
        fn work(&mut self, _channel_controller: &mut dyn ChannelController) {
            self.tx.send(MockEvent::Work).unwrap();
        }
        fn exit_idle(&mut self, _channel_controller: &mut dyn ChannelController) {
            self.tx.send(MockEvent::ExitIdle).unwrap();
        }
    }
}
