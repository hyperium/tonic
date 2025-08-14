use std::sync::Mutex;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    panic,
    sync::Arc,
};

use tokio::sync::mpsc;

use crate::client::{
    load_balancing::{
        child_manager::{
            Child, ChildManager, ChildUpdate, ChildWorkScheduler, ResolverUpdateSharder,
        },
        test_utils::{
            self, FakeChannel, MockLbPolicy, MockPicker, MockPolicyBuilder, TestEvent,
            TestWorkScheduler,
        },
        ChannelController, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState, ParsedJsonLbConfig,
        PickResult, Picker, Subchannel, SubchannelState, GLOBAL_LB_REGISTRY,
    },
    name_resolution::{Address, Endpoint, ResolverUpdate},
    service_config::ServiceConfig,
    ConnectivityState,
};
use crate::rt::{default_runtime, Runtime};

struct EndpointSharder {
    builder: Arc<dyn LbPolicyBuilder>,
}

impl ResolverUpdateSharder<Endpoint> for EndpointSharder {
    fn shard_update(
        &self,
        resolver_update: ResolverUpdate,
    ) -> Result<Box<dyn Iterator<Item = ChildUpdate<Endpoint>>>, Box<dyn Error + Send + Sync>> {
        let mut endpoint_to_child_map = HashMap::new();
        for endpoint in resolver_update.endpoints.clone().unwrap().iter() {
            let child_update = ChildUpdate {
                child_identifier: endpoint.clone(),
                child_policy_builder: self.builder.clone(),
                child_update: ResolverUpdate {
                    attributes: resolver_update.attributes.clone(),
                    endpoints: Ok(vec![endpoint.clone()]),
                    service_config: resolver_update.service_config.clone(),
                    resolution_note: resolver_update.resolution_note.clone(),
                },
            };
            endpoint_to_child_map.insert(endpoint.clone(), child_update);
        }
        Ok(Box::new(endpoint_to_child_map.into_values()))
    }
}

fn setup() -> (
    mpsc::UnboundedReceiver<TestEvent>,
    Box<ChildManager<Endpoint>>,
    Box<dyn ChannelController>,
) {
    let (tx_events, rx_events) = mpsc::unbounded_channel::<TestEvent>();
    let work_scheduler = Arc::new(TestWorkScheduler {
        tx_events: tx_events.clone(),
    });

    let tcc = Box::new(FakeChannel {
        tx_events: tx_events.clone(),
    });
    GLOBAL_LB_REGISTRY.add_builder(MockPolicyBuilder { name: "one" });
    let endpoint_sharder = EndpointSharder {
        builder: GLOBAL_LB_REGISTRY.get_policy("one").unwrap(),
    };
    let child_manager = ChildManager::new(Box::new(endpoint_sharder), default_runtime());
    (rx_events, Box::new(child_manager), tcc)
}

fn create_n_endpoints_with_k_addresses(n: usize, k: usize) -> Vec<Endpoint> {
    let mut addresses = Vec::new();
    let mut endpoints = Vec::new();
    for i in 0..n {
        let mut n_addresses = Vec::new();
        for j in 0..k {
            n_addresses.push(Address {
                address: format!("{}.{}.{}.{}:{}", j + i, j + i, j + i, j + i, j + i)
                    .to_string()
                    .into(),
                ..Default::default()
            });
        }
        addresses.push(n_addresses);
    }
    for i in 0..n {
        endpoints.push(Endpoint {
            addresses: addresses[i].clone(),
            ..Default::default()
        })
    }
    endpoints
}

// Mock child where the connectivity_state can be directly set
fn mock_child(id: Endpoint, connectivity_state: ConnectivityState) -> Child<Endpoint> {
    let pending_work = Arc::new(Mutex::new(HashSet::new()));
    Child {
        identifier: id,
        policy: Box::new(MockLbPolicy::new(connectivity_state)),
        state: LbState {
            connectivity_state: connectivity_state,
            picker: Arc::new(MockPicker::new("mock-picker")),
        },
        work_scheduler: Arc::new(ChildWorkScheduler {
            pending_work,
            idx: Mutex::new(Some(0)),
        }),
    }
}

// Tests the scenario where there is a READY child.
// The child_manager should report READY.
#[tokio::test]
async fn childmanager_ready_has_highest_priority() {
    let (_rx, mut child_manager, _tcc) = setup();
    let endpoints = create_n_endpoints_with_k_addresses(4, 1);

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::Connecting),
        mock_child(endpoints[1].clone(), ConnectivityState::Idle),
        mock_child(endpoints[2].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[3].clone(), ConnectivityState::Ready),
    ];

    assert_eq!(child_manager.aggregate_states(), ConnectivityState::Ready);
}

// Tests the scenario where there is a CONNECTING child but no READY children.
// The child_manager should report CONNECTING.
#[tokio::test]
async fn childmanager_connecting_has_higher_priority() {
    let (_rx, mut child_manager, _tcc) = setup();
    let endpoints = create_n_endpoints_with_k_addresses(4, 1);

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::Connecting),
        mock_child(endpoints[1].clone(), ConnectivityState::Idle),
        mock_child(endpoints[2].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[3].clone(), ConnectivityState::TransientFailure),
    ];

    assert_eq!(
        child_manager.aggregate_states(),
        ConnectivityState::Connecting
    );
}

// Tests the scenario where there is one IDLE child but no CONNECTING or READY children.
// The child_manager should report IDLE.
#[tokio::test]
async fn childmanager_idle_has_higher_priority() {
    let (_rx, mut child_manager, _tcc) = setup();
    let endpoints = create_n_endpoints_with_k_addresses(4, 1);

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[1].clone(), ConnectivityState::Idle),
        mock_child(endpoints[2].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[3].clone(), ConnectivityState::TransientFailure),
    ];

    assert_eq!(child_manager.aggregate_states(), ConnectivityState::Idle);
}

// Tests the scenario where all children are in TRANSIENT FAILURE.
// The child_manager should report TRANSIENT FAILURE.
#[tokio::test]
async fn childmanager_reports_transient_failure() {
    let (_rx, mut child_manager, _tcc) = setup();
    let endpoints = create_n_endpoints_with_k_addresses(4, 1);

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[1].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[2].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[3].clone(), ConnectivityState::TransientFailure),
    ];

    assert_eq!(
        child_manager.aggregate_states(),
        ConnectivityState::TransientFailure
    );
}

// Tests the scenario where a child reports READY after CONNECTING.
// The child_manager should report CONNECTING first then READY.
#[tokio::test]
async fn childmanager_connecting_then_ready() {
    let (_rx, mut child_manager, _tcc) = setup();
    let endpoints = create_n_endpoints_with_k_addresses(4, 1);

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::Connecting),
        mock_child(endpoints[1].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[2].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[3].clone(), ConnectivityState::TransientFailure),
    ];

    assert_eq!(
        child_manager.aggregate_states(),
        ConnectivityState::Connecting
    );

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::Ready),
        mock_child(endpoints[1].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[2].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[3].clone(), ConnectivityState::TransientFailure),
    ];

    assert_eq!(child_manager.aggregate_states(), ConnectivityState::Ready);
}

// Tests the scenario where a child reports READY but the other children states.
// The child_manager should keep reporting READY.
#[tokio::test]
async fn childmanager_stays_in_ready() {
    let (_rx, mut child_manager, _tcc) = setup();
    let endpoints = create_n_endpoints_with_k_addresses(4, 1);

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::Ready),
        mock_child(endpoints[1].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[2].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[3].clone(), ConnectivityState::TransientFailure),
    ];

    assert_eq!(child_manager.aggregate_states(), ConnectivityState::Ready);

    child_manager.children = vec![
        mock_child(endpoints[0].clone(), ConnectivityState::Ready),
        mock_child(endpoints[1].clone(), ConnectivityState::TransientFailure),
        mock_child(endpoints[2].clone(), ConnectivityState::Idle),
        mock_child(endpoints[3].clone(), ConnectivityState::Connecting),
    ];

    assert_eq!(child_manager.aggregate_states(), ConnectivityState::Ready);

    assert_eq!(child_manager.aggregate_states(), ConnectivityState::Ready);
}
