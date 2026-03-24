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

use std::fmt::Debug;
use std::sync::Arc;

use rand::seq::SliceRandom;
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
use crate::rt::GrpcRuntime;

pub(crate) static POLICY_NAME: &str = "pick_first";

#[derive(Debug, serde::Deserialize, Clone)]
pub(crate) struct PickFirstConfig {
    #[serde(rename = "shuffleAddressList")]
    pub shuffle_address_list: bool,
}

#[derive(Debug)]
struct Builder {}

impl LbPolicyBuilder for Builder {
    type LbPolicy = PickFirstPolicy;

    fn build(&self, options: LbPolicyOptions) -> Self::LbPolicy {
        PickFirstPolicy {
            work_scheduler: options.work_scheduler,
            runtime: options.runtime,
            subchannels: Vec::default(),
            selected: None,
            current_index: 0,
            connectivity_state: ConnectivityState::Connecting,
            config: None,
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
    super::GLOBAL_LB_REGISTRY.add_builder(Builder {})
}

#[derive(Debug)]
pub(crate) struct PickFirstPolicy {
    work_scheduler: Arc<dyn WorkScheduler>,
    runtime: GrpcRuntime,
    subchannels: Vec<Arc<dyn Subchannel>>,
    selected: Option<Arc<dyn Subchannel>>,
    current_index: usize,
    connectivity_state: ConnectivityState,
    config: Option<PickFirstConfig>,
}

impl PickFirstPolicy {
    fn start_connection_pass(&mut self, channel_controller: &mut dyn ChannelController) {
        self.current_index = 0;
        self.selected = None;
        if let Some(sc) = self.subchannels.get(0) {
            self.connectivity_state = ConnectivityState::Connecting;
            sc.connect();
            channel_controller.update_picker(LbState {
                connectivity_state: ConnectivityState::Connecting,
                picker: Arc::new(QueuingPicker {}),
            });
        } else {
            // This should have been handled in resolver_update, but just in case.
            self.connectivity_state = ConnectivityState::TransientFailure;
            channel_controller.update_picker(LbState {
                connectivity_state: ConnectivityState::TransientFailure,
                picker: Arc::new(FailingPicker {
                    error: "no addresses available".to_string(),
                }),
            });
        }
    }

    fn rebuild_subchannels(
        &mut self,
        new_addresses: Vec<Address>,
        channel_controller: &mut dyn ChannelController,
    ) {
        // Map existing subchannels by address.
        let mut existing_map: std::collections::HashMap<Address, Arc<dyn Subchannel>> = self
            .subchannels
            .drain(..)
            .map(|sc| (sc.address(), sc))
            .collect();

        // Build the new list, pulling from the map where possible to preserve backoff state.
        self.subchannels = new_addresses
            .into_iter()
            .map(|addr| {
                existing_map
                    .remove(&addr)
                    .unwrap_or_else(|| channel_controller.new_subchannel(&addr).0)
            })
            .collect();
    }

    fn update_config(&mut self, config: Option<&PickFirstConfig>) -> Result<(), String> {
        if let Some(lb_config) = config {
            self.config = Some(lb_config.clone());
        }
        Ok(())
    }

    // Converts the update endpoints to an address list, erroring if empty.
    // Pick first doesn't care about the Endpoint attributes; these are dropped.
    // If the configuration is set, this list will be shuffled.
    fn compile_address(
        &mut self,
        endpoints: Vec<Endpoint>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<Vec<Address>, String> {
        let mut addresses = endpoints
            .into_iter()
            .flat_map(|e| e.addresses)
            .collect::<Vec<_>>();

        if addresses.is_empty() {
            self.set_transient_failure(channel_controller, "empty address list".to_string())?;
        }

        if self
            .config
            .as_ref()
            .map(|c| c.shuffle_address_list)
            .unwrap_or(false)
        {
            let mut rng = rand::rng();
            addresses.shuffle(&mut rng);
        }

        Ok(addresses)
    }

    // Sets state to TRANSIENT_FAILURE and updates picker with error.
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
}

impl LbPolicy for PickFirstPolicy {
    type LbConfig = PickFirstConfig;

    fn resolver_update(
        &mut self,
        update: ResolverUpdate,
        config: Option<&Self::LbConfig>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<(), String> {
        self.update_config(config)?;
        let new_addresses = self.compile_address(
            update.endpoints.map_err(|e| e.to_string())?,
            channel_controller,
        )?;

        // Stickiness: Check if currently selected subchannel is in the new list.
        if let Some(ref selected) = self.selected {
            if new_addresses.contains(&selected.address()) {
                self.rebuild_subchannels(new_addresses, channel_controller);
                return Ok(());
            }
        }

        self.rebuild_subchannels(new_addresses, channel_controller);
        self.start_connection_pass(channel_controller);

        Ok(())
    }

    fn subchannel_update(
        &mut self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn ChannelController,
    ) {
        // 1. If we have a selected subchannel, only care about updates from it.
        if let Some(ref selected) = self.selected {
            if selected.address() == subchannel.address() {
                if state.connectivity_state != ConnectivityState::Ready {
                    // Lost connection: Go IDLE as per Vanilla design.
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

        // 2. Otherwise, check if this is from the subchannel we are currently attempting.
        if let Some(attempting) = self.subchannels.get(self.current_index) {
            if attempting.address() == subchannel.address() {
                match state.connectivity_state {
                    ConnectivityState::Ready => {
                        self.selected = Some(subchannel.clone());
                        self.connectivity_state = ConnectivityState::Ready;
                        channel_controller.update_picker(LbState {
                            connectivity_state: ConnectivityState::Ready,
                            picker: Arc::new(OneSubchannelPicker { sc: subchannel }),
                        });
                    }
                    ConnectivityState::TransientFailure => {
                        // Move to next address
                        self.current_index += 1;
                        if self.current_index < self.subchannels.len() {
                            let next_sc = &self.subchannels[self.current_index];
                            next_sc.connect();
                        } else {
                            // Exhausted: TRANSIENT_FAILURE and request re-resolution.
                            self.connectivity_state = ConnectivityState::TransientFailure;
                            let error = state
                                .last_connection_error
                                .as_ref()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "all addresses failed".to_string());

                            channel_controller.update_picker(LbState {
                                connectivity_state: ConnectivityState::TransientFailure,
                                picker: Arc::new(FailingPicker { error }),
                            });
                            channel_controller.request_resolution();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        if self.connectivity_state == ConnectivityState::Idle {
            self.exit_idle(channel_controller);
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

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::load_balancing::test_utils::{
        TestChannelController, TestEvent, TestWorkScheduler,
    };
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn setup() -> (
        mpsc::UnboundedReceiver<TestEvent>,
        PickFirstPolicy,
        Box<TestChannelController>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let work_scheduler = Arc::new(TestWorkScheduler {
            tx_events: tx.clone(),
        });
        let runtime = crate::rt::default_runtime();
        let policy = PickFirstPolicy {
            work_scheduler,
            runtime,
            subchannels: Vec::new(),
            selected: None,
            current_index: 0,
            connectivity_state: ConnectivityState::Idle,
            config: None,
        };
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
        let (mut rx, mut policy, mut controller) = setup();
        let endpoints = create_endpoints(vec!["addr1", "addr2"]);
        let update = ResolverUpdate {
            endpoints: Ok(endpoints),
            ..Default::default()
        };

        policy
            .resolver_update(update, None, controller.as_mut())
            .unwrap();

        // Expect NewSubchannel x2, Connect, UpdatePicker(Connecting)
        rx.recv().await;
        rx.recv().await;
        rx.recv().await;
        rx.recv().await;

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
        match rx.recv().await.unwrap() {
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
        let (mut rx, mut policy, mut controller) = setup();
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
        rx.recv().await;
        rx.recv().await;
        rx.recv().await;
        rx.recv().await;

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
        match rx.recv().await.unwrap() {
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

        match rx.recv().await.unwrap() {
            TestEvent::UpdatePicker(state) => {
                assert_eq!(state.connectivity_state, ConnectivityState::Ready)
            }
            other => panic!("unexpected event {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pick_first_stickiness() {
        let (mut rx, mut policy, mut controller) = setup();
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
        rx.recv().await;
        rx.recv().await;
        rx.recv().await;
        rx.recv().await;

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
        match rx.recv().await.unwrap() {
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

        // Should create subchannel for addr3 (addr1 and addr2 are re-used)
        match rx.recv().await.unwrap() {
            TestEvent::NewSubchannel(sc) => assert_eq!(sc.address().address.to_string(), "addr3"),
            other => panic!("unexpected event {:?}", other),
        }

        // Should NOT have any more events (no Connect, no UpdatePicker) because it is sticky
        tokio::select! {
            e = rx.recv() => panic!("unexpected event {:?}", e),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        assert_eq!(
            policy.selected.as_ref().unwrap().address().address.to_string(),
            "addr1"
        );
    }

    #[tokio::test]
    async fn test_pick_first_exhaustion() {
        let (mut rx, mut policy, mut controller) = setup();
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
        rx.recv().await;
        rx.recv().await;
        rx.recv().await;

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
        match rx.recv().await.unwrap() {
            TestEvent::UpdatePicker(state) => assert_eq!(
                state.connectivity_state,
                ConnectivityState::TransientFailure
            ),
            other => panic!("unexpected event {:?}", other),
        }

        // Should request re-resolution
        match rx.recv().await.unwrap() {
            TestEvent::RequestResolution => {}
            other => panic!("unexpected event {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pick_first_shuffling() {
        let (mut _rx, mut policy, mut controller) = setup();

        // Enable shuffling in config
        let config = PickFirstConfig {
            shuffle_address_list: true,
        };

        // Provide 10 addresses
        let addrs: Vec<String> = (0..100).map(|i| format!("addr{}", i)).collect();
        let endpoints = create_endpoints(addrs.iter().map(|s| s.as_str()).collect());

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

        // While non-deterministic, the probability of 100 items remaining in exact order is 1/(100!).
        // Would need to add a RNG to the options to make this deterministic.
        assert_ne!(resulting_addrs, addrs, "Addresses were not shuffled");
    }
}
