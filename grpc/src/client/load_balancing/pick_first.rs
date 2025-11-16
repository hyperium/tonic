use std::{error::Error, sync::Arc, time::Duration};

use tonic::metadata::MetadataMap;

use crate::{
    client::{
        load_balancing::{LbPolicy, LbPolicyBuilder, LbState},
        name_resolution::{Address, ResolverUpdate},
        ConnectivityState,
    },
    rt::Runtime,
    service::Request,
};

use super::{
    ChannelController, LbConfig, LbPolicyOptions, Pick, PickResult, Picker, Subchannel,
    SubchannelState, WorkScheduler,
};

pub(crate) static POLICY_NAME: &str = "pick_first";

#[derive(Debug)]
struct Builder {}

impl LbPolicyBuilder for Builder {
    fn build(&self, options: LbPolicyOptions) -> Box<dyn LbPolicy> {
        Box::new(PickFirstPolicy {
            work_scheduler: options.work_scheduler,
            subchannel: None,
            next_addresses: Vec::default(),
            runtime: options.runtime,
        })
    }

    fn name(&self) -> &'static str {
        POLICY_NAME
    }
}

pub(crate) fn reg() {
    super::GLOBAL_LB_REGISTRY.add_builder(Builder {})
}

#[derive(Debug)]
struct PickFirstPolicy {
    work_scheduler: Arc<dyn WorkScheduler>,
    subchannel: Option<Arc<dyn Subchannel>>,
    next_addresses: Vec<Address>,
    runtime: Arc<dyn Runtime>,
}

impl LbPolicy for PickFirstPolicy {
    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut addresses = update
            .endpoints
            .unwrap()
            .into_iter()
            .next()
            .ok_or("no endpoints")?
            .addresses;

        let address = addresses.pop().ok_or("no addresses")?;

        let sc = channel_controller.new_subchannel(&address);
        sc.connect();
        self.subchannel = Some(sc);

        self.next_addresses = addresses;
        let work_scheduler = self.work_scheduler.clone();
        let runtime = self.runtime.clone();
        // TODO: Implement Drop that cancels this task.
        self.runtime.spawn(Box::pin(async move {
            runtime.sleep(Duration::from_millis(200)).await;
            work_scheduler.schedule_work();
        }));
        // TODO: return a picker that queues RPCs.
        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        // Assume the update is for our subchannel.
        if state.connectivity_state == ConnectivityState::Ready {
            channel_controller.update_picker(LbState {
                connectivity_state: ConnectivityState::Ready,
                picker: Arc::new(OneSubchannelPicker {
                    sc: self.subchannel.as_ref().unwrap().clone(),
                }),
            });
        }
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {}

    fn exit_idle(&mut self, _channel_controller: &mut dyn ChannelController) {
        todo!("implement exit_idle")
    }
}

#[derive(Debug)]
struct OneSubchannelPicker {
    sc: Arc<dyn Subchannel>,
}

impl Picker for OneSubchannelPicker {
    fn pick(&self, request: &Request) -> PickResult {
        PickResult::Pick(Pick {
            subchannel: self.sc.clone(),
            // on_complete: None,
            metadata: MetadataMap::new(),
            on_complete: None,
        })
    }
}
