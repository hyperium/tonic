use super::connection::Connection;
use crate::transport::Endpoint;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::discover::{Change, Discover};

#[derive(Debug)]
pub struct ServiceList {
    list: VecDeque<Endpoint>,
    i: usize,
}

impl ServiceList {
    pub fn new(list: Vec<Endpoint>) -> Self {
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

                match Connection::new(endpoint) {
                    Ok(svc) => Poll::Ready(Ok(Change::Insert(i, svc))),
                    Err(e) => Poll::Ready(Err(e)),
                }
            }
            None => Poll::Pending,
        }
    }
}
