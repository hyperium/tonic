use super::connection::Connection;
use crate::transport::Endpoint;
use std::hash::Hash;
use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::stream::Stream;
use tower::discover::{Change, Discover};

pub(crate) struct ServiceList {
    list: VecDeque<Endpoint>,
    connecting:
        Option<Pin<Box<dyn Future<Output = Result<Connection, crate::Error>> + Send + 'static>>>,
    i: usize,
}

pub(crate) struct DynamicServiceStream<K: Hash + Eq + Clone + Unpin> {
    changes: Box<dyn Stream<Item = Change<K, Endpoint>> + Unpin + Send + 'static>,
    connecting: Option<(
        K,
        Pin<Box<dyn Future<Output = Result<Connection, crate::Error>> + Send + 'static>>,
    )>,
}

impl ServiceList {
    pub(crate) fn new(list: Vec<Endpoint>) -> Self {
        Self {
            list: list.into(),
            connecting: None,
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
                let mut http = hyper::client::connect::HttpConnector::new();
                http.set_nodelay(endpoint.tcp_nodelay);
                http.set_keepalive(endpoint.tcp_keepalive);

                let fut = Connection::new(http, endpoint);
                self.connecting = Some(Box::pin(fut));
            } else {
                return Poll::Pending;
            }
        }
    }
}

impl fmt::Debug for ServiceList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceList")
            .field("list", &self.list)
            .finish()
    }
}

impl<K: Hash + Eq + Clone + Unpin> DynamicServiceStream<K> {
    pub(crate) fn new(
        changes: impl Stream<Item = Change<K, Endpoint>> + Unpin + Send + 'static,
    ) -> Self {
        Self {
            changes: Box::new(changes),
            connecting: None,
        }
    }
}

impl<K: Hash + Eq + Clone + Unpin> Discover for DynamicServiceStream<K> {
    type Key = K;
    type Service = Connection;
    type Error = crate::Error;

    fn poll_discover(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Change<Self::Key, Self::Service>, Self::Error>> {
        loop {
            if let Some((key, connecting)) = &mut self.connecting {
                let svc = futures_core::ready!(Pin::new(connecting).poll(cx))?;
                let key = key.to_owned();
                self.connecting = None;
                let change = Ok(Change::Insert(key, svc));
                return Poll::Ready(change);
            };

            let c = &mut self.changes;

            match Pin::new(&mut *c).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    let transport_type = crate::transport::error::Kind::Transport;
                    let err = Box::new(crate::transport::Error::new(transport_type));
                    return Poll::Ready(Err(err));
                }
                Poll::Ready(Some(change)) => match change {
                    Change::Insert(k, endpoint) => {
                        let mut http = hyper::client::connect::HttpConnector::new();
                        http.set_nodelay(endpoint.tcp_nodelay);
                        http.set_keepalive(endpoint.tcp_keepalive);
                        let fut = Connection::new(http, endpoint);
                        self.connecting = Some((k, Box::pin(fut)));
                        continue;
                    }
                    Change::Remove(k) => return Poll::Ready(Ok(Change::Remove(k))),
                },
            }
        }
    }
}
