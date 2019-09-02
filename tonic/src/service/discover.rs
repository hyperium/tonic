use super::connect::Connection;
use http::Uri;
use std::collections::VecDeque;
use std::task::{Context, Poll};
use tower_discover::{Change, Discover};

#[derive(Debug)]
pub struct ServiceList {
    list: VecDeque<Uri>,
    i: usize,
}

impl ServiceList {
    pub fn new(list: Vec<Uri>) -> Self {
        Self {
            list: list.into(),
            i: 0,
        }
    }
}

impl Discover for ServiceList {
    type Key = usize;
    type Service = Connection;
    type Error = hyper::Error;

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Change<Self::Key, Self::Service>, Self::Error>> {
        match self.list.pop_front() {
            Some(uri) => {
                let i = self.i;
                self.i += 1;
                let service = Connection::new(uri);
                Poll::Ready(Ok(Change::Insert(i, service)))
            }
            None => Poll::Pending,
        }
    }
}
