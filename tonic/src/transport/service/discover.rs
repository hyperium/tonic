use super::connection::Connection;
use crate::transport::Endpoint;
use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::discover::{Change, Discover};

pub(crate) struct ServiceList<C> {
    list: VecDeque<Endpoint>,
    connector: C,
    connecting:
        Option<Pin<Box<dyn Future<Output = Result<Connection, crate::Error>> + Send + 'static>>>,
    i: usize,
}

impl<C> ServiceList<C> {
    pub(crate) fn new(list: Vec<Endpoint>, connector: C) -> Self {
        Self {
            list: list.into(),
            connector,
            connecting: None,
            i: 0,
        }
    }
}

impl<C> Discover for ServiceList<C>
where
    C: tower_make::MakeConnection<hyper::Uri> + Send + Clone + Unpin + 'static,
    C::Connection: Unpin + Send + 'static,
    C::Future: Send + 'static,
    C::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send,
{
    type Key = usize;
    type Service = Connection;
    type Error = crate::Error;

    fn poll_discover(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Change<Self::Key, Self::Service>, Self::Error>> {
        loop {
            if let Some(connecting) = &mut self.connecting {
                let svc = futures_core::ready!(Pin::new(connecting).poll(cx))?;
                self.connecting = None;

                let i = self.i;
                self.i += 1;

                let change = Ok(Change::Insert(i, svc));

                return Poll::Ready(change);
            }

            if let Some(endpoint) = self.list.pop_front() {
                let c = &self.connector;
                let fut = Connection::new(endpoint, c.clone());
                self.connecting = Some(Box::pin(fut));
            } else {
                return Poll::Pending;
            }
        }
    }
}

impl<C> fmt::Debug for ServiceList<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceList")
            .field("list", &self.list)
            .finish()
    }
}
