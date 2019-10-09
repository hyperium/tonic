use super::connection::Connection;
use crate::transport::Endpoint;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::discover::{Change, Discover};

#[derive(Debug)]
pub(crate) struct ServiceList {
    list: VecDeque<Endpoint>,
    i: usize,
}

impl ServiceList {
    pub(crate) fn new(list: Vec<Endpoint>) -> Self {
        Self {
            list: list.into(),
            i: 0,
        }
    }
}

impl Discover for ServiceList {
    type Key = usize;
    type Service = Connection;
    type Error = crate::Error;

    fn poll_discover(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Change<Self::Key, Self::Service>, Self::Error>> {
        match self.list.pop_front() {
            Some(endpoint) => {
                let i = self.i;
                self.i += 1;

                let svc = Connection::new(endpoint);
                let change = Ok(Change::Insert(i, svc));

                Poll::Ready(change)
            }
            None => Poll::Pending,
        }
    }
}
