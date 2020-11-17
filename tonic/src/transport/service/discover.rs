use super::super::service;
use super::connection::Connection;
use crate::transport::Endpoint;

use std::{
    hash::Hash,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{stream::Stream, sync::mpsc::Receiver};

use tower::discover::{Change, Discover};

type DiscoverResult<K, S, E> = Result<Change<K, S>, E>;

pub(crate) struct DynamicServiceStream<K: Hash + Eq + Clone> {
    changes: Receiver<Change<K, Endpoint>>,
}

impl<K: Hash + Eq + Clone> DynamicServiceStream<K> {
    pub(crate) fn new(changes: Receiver<Change<K, Endpoint>>) -> Self {
        Self { changes }
    }
}

impl<K: Hash + Eq + Clone> Discover for DynamicServiceStream<K> {
    type Key = K;
    type Service = Connection;
    type Error = crate::Error;

    fn poll_discover(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<DiscoverResult<Self::Key, Self::Service, Self::Error>> {
        loop {
            let c = &mut self.changes;
            return match Pin::new(&mut *c).poll_next(cx) {
                Poll::Pending | Poll::Ready(None) => Poll::Pending,
                Poll::Ready(Some(change)) => match change {
                    Change::Insert(k, endpoint) => {
                        let mut http = hyper::client::connect::HttpConnector::new();
                        http.set_nodelay(endpoint.tcp_nodelay);
                        http.set_keepalive(endpoint.tcp_keepalive);
                        http.enforce_http(false);
                        #[cfg(feature = "tls")]
                        let connector = service::connector(http, endpoint.tls.clone());

                        #[cfg(not(feature = "tls"))]
                        let connector = service::connector(http);
                        let connection = Connection::lazy(connector, endpoint);
                        let change = Ok(Change::Insert(k, connection));
                        Poll::Ready(change)
                    }
                    Change::Remove(k) => Poll::Ready(Ok(Change::Remove(k))),
                },
            };
        }
    }
}

impl<K: Hash + Eq + Clone> Unpin for DynamicServiceStream<K> {}
