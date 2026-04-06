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

pub(crate) static POLICY_NAME: &str = "pick_first";

type ShufflerFn = dyn Fn(&mut [Address]) + Send + Sync + 'static;

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
            subchannels: Vec::default(),
            selected: None,
            current_index: 0,
            connectivity_state: ConnectivityState::Connecting,
            config: None,
            last_resolver_error: None,
            last_connection_error: None,
            shuffler: Arc::new(|addrs| {
                let mut rng = rand::rng();
                addrs.shuffle(&mut rng);
            }),
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
    subchannels: Vec<Arc<dyn Subchannel>>,
    selected: Option<Arc<dyn Subchannel>>,
    current_index: usize,
    connectivity_state: ConnectivityState,
    config: Option<PickFirstConfig>,

    // Detailed error tracking inspired by PR #2340
    last_resolver_error: Option<String>,
    last_connection_error: Option<String>,

    // Injectable shuffler for deterministic testing
    shuffler: Arc<ShufflerFn>,
}

impl Debug for PickFirstPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickFirstPolicy")
            .field("subchannels", &self.subchannels)
            .field("selected", &self.selected)
            .field("current_index", &self.current_index)
            .field("connectivity_state", &self.connectivity_state)
            .field("config", &self.config)
            .field("last_resolver_error", &self.last_resolver_error)
            .field("last_connection_error", &self.last_connection_error)
            .finish()
    }
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
            let error = self
                .last_resolver_error
                .clone()
                .unwrap_or_else(|| "no addresses available".to_string());
            self.set_transient_failure(channel_controller, error)
                .unwrap_or(());
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

    // Converts the update endpoints to an address list.
    // Includes de-duplication logic inspired by PR #2340
    fn compile_address(
        &mut self,
        endpoints: Vec<Endpoint>,
        channel_controller: &mut dyn ChannelController,
    ) -> Result<Vec<Address>, String> {
        let mut addresses = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for endpoint in endpoints {
            for address in endpoint.addresses {
                if seen.insert(address.clone()) {
                    addresses.push(address);
                }
            }
        }

        if addresses.is_empty() {
            let error = self
                .last_resolver_error
                .clone()
                .unwrap_or_else(|| "empty address list".to_string());
            return self
                .set_transient_failure(channel_controller, error)
                .map(|_| vec![]);
        }

        if self
            .config
            .as_ref()
            .map(|c| c.shuffle_address_list)
            .unwrap_or(false)
        {
            (self.shuffler)(&mut addresses);
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

        match update.endpoints {
            Ok(endpoints) => {
                let new_addresses = self.compile_address(endpoints, channel_controller)?;

                // Stickiness: Check if currently selected subchannel is in the new list.
                if let Some(ref selected) = self.selected {
                    if new_addresses.contains(&selected.address()) {
                        self.rebuild_subchannels(new_addresses, channel_controller);
                        return Ok(());
                    }
                }

                self.rebuild_subchannels(new_addresses, channel_controller);
                self.start_connection_pass(channel_controller);
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

                            self.last_connection_error = Some(error.clone());
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
        let mut policy = PickFirstBuilder {}.build(LbPolicyOptions {
            work_scheduler,
            runtime,
        });

        // Manual override for deterministic shuffling in tests
        policy.shuffler = Arc::new(|addrs| {
            addrs.reverse(); // Deterministic "shuffle"
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
    async fn test_pick_first_shuffling_deterministic() {
        let (mut _rx, mut policy, mut controller) = setup();

        // Enable shuffling in config
        let config = PickFirstConfig {
            shuffle_address_list: true,
        };

        // Provide addresses in order
        let addrs = vec!["addr1", "addr2", "addr3"];
        let endpoints = create_endpoints(addrs.clone());

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

        // Our mock shuffler reverses the list
        let mut expected = addrs.clone();
        expected.reverse();
        assert_eq!(resulting_addrs, expected, "Deterministic shuffling failed");
    }

    #[tokio::test]
    async fn test_pick_first_duplicate_de_duplication() {
        let (mut rx, mut policy, mut controller) = setup();

        // Create endpoints with duplicates
        let endpoints = vec![
            Endpoint {
                addresses: vec![
                    Address {
                        address: "addr1".to_string().into(),
                        ..Default::default()
                    },
                    Address {
                        address: "addr1".to_string().into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Endpoint {
                addresses: vec![
                    Address {
                        address: "addr2".to_string().into(),
                        ..Default::default()
                    },
                    Address {
                        address: "addr1".to_string().into(),
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
        rx.recv().await; // NewSubchannel(addr1)
        rx.recv().await; // NewSubchannel(addr2)

        // Verify no 3rd subchannel was created
        tokio::select! {
            e = rx.recv() => match e.unwrap() {
                TestEvent::NewSubchannel(_) => panic!("Duplicate subchannel created"),
                _ => {} // Connect and UpdatePicker are expected
            },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        assert_eq!(policy.subchannels.len(), 2, "De-duplication failed");
    }

    #[tokio::test]
    async fn test_pick_first_config_parsing() {
        let builder = PickFirstBuilder {};

        // Test valid config
        let json = r#"{"shuffleAddressList": true}"#;
        let parsed = ParsedJsonLbConfig::new(json).unwrap();
        let config = builder.parse_config(&parsed).unwrap().unwrap();
        assert!(config.shuffle_address_list);

        // Test invalid JSON type
        let json_invalid = r#"{"shuffleAddressList": "not-a-bool"}"#;
        let parsed_invalid = ParsedJsonLbConfig::new(json_invalid).unwrap();
        assert!(builder.parse_config(&parsed_invalid).is_err());
    }
}
