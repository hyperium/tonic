use crate::client::{
    load_balancing::{
        ChannelController, ExternalSubchannel, ForwardingSubchannel, LbState, Subchannel,
        WorkScheduler,
    },
    name_resolution::Address,
};
use crate::service::{Message, Request, Response, Service};
use std::{
    fmt::Display,
    hash::{Hash, Hasher},
    ops::Add,
    sync::Arc,
};
use tokio::{
    sync::{mpsc, Notify},
    task::AbortHandle,
};

pub(crate) struct EmptyMessage {}
impl Message for EmptyMessage {}
pub(crate) fn new_request() -> Request {
    Request::new(Box::pin(tokio_stream::once(
        Box::new(EmptyMessage {}) as Box<dyn Message>
    )))
}

// A test subchannel that forwards connect calls to a channel.
// This allows tests to verify when a subchannel is asked to connect.
pub(crate) struct TestSubchannel {
    address: Address,
    tx_connect: mpsc::UnboundedSender<TestEvent>,
}

impl TestSubchannel {
    fn new(address: Address, tx_connect: mpsc::UnboundedSender<TestEvent>) -> Self {
        Self {
            address,
            tx_connect,
        }
    }
}

impl ForwardingSubchannel for TestSubchannel {
    fn delegate(&self) -> Arc<dyn Subchannel> {
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
        self.address == other.address
    }
}
impl Eq for TestSubchannel {}

pub(crate) enum TestEvent {
    NewSubchannel(Address, Arc<dyn Subchannel>),
    UpdatePicker(LbState),
    RequestResolution,
    Connect(Address),
    ScheduleWork,
}

impl Display for TestEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NewSubchannel(addr, _) => write!(f, "NewSubchannel({})", addr),
            Self::UpdatePicker(state) => write!(f, "UpdatePicker({})", state.connectivity_state),
            Self::RequestResolution => write!(f, "RequestResolution"),
            Self::Connect(addr) => write!(f, "Connect({})", addr.address.to_string()),
            Self::ScheduleWork => write!(f, "ScheduleWork"),
        }
    }
}

// A test channel controller that forwards calls to a channel.  This allows
// tests to verify when a channel controller is asked to create subchannels or
// update the picker.
pub(crate) struct TestChannelController {
    pub tx_events: mpsc::UnboundedSender<TestEvent>,
}

impl ChannelController for TestChannelController {
    fn new_subchannel(&mut self, address: &Address) -> Arc<dyn Subchannel> {
        println!("new_subchannel called for address {}", address);
        let notify = Arc::new(Notify::new());
        let subchannel: Arc<dyn Subchannel> =
            Arc::new(TestSubchannel::new(address.clone(), self.tx_events.clone()));
        self.tx_events
            .send(TestEvent::NewSubchannel(
                address.clone(),
                subchannel.clone(),
            ))
            .unwrap();
        subchannel
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

pub(crate) struct TestWorkScheduler {
    pub tx_events: mpsc::UnboundedSender<TestEvent>,
}

impl WorkScheduler for TestWorkScheduler {
    fn schedule_work(&self) {
        self.tx_events.send(TestEvent::ScheduleWork).unwrap();
    }
}
