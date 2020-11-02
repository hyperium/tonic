use super::super::{service, BoxFuture};
use super::connection::Connection;
use crate::transport::Endpoint;

use std::{
    future::Future,
    hash::Hash,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{stream::Stream, sync::mpsc::Receiver};

use tower::discover::Change;

type DiscoverResult<K, S, E> = Result<Change<K, S>, E>;

pub(crate) struct DynamicServiceStream<K: Hash + Eq + Clone> {
    changes: Receiver<Change<K, Endpoint>>,
    connecting: Option<(K, BoxFuture<Connection, crate::Error>)>,
}

impl<K: Hash + Eq + Clone> DynamicServiceStream<K> {
    pub(crate) fn new(changes: Receiver<Change<K, Endpoint>>) -> Self {
        Self {
            changes,
            connecting: None,
        }
    }
}

impl<K: Hash + Eq + Clone> Stream for DynamicServiceStream<K> {
    type Item = DiscoverResult<K, Connection, crate::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some((key, connecting)) = &mut self.connecting {
                let svc = futures_core::ready!(Pin::new(connecting).poll(cx))?;
                let key = key.to_owned();
                self.connecting = None;
                let change = Ok(Change::Insert(key, svc));
                return Poll::Ready(Some(change));
            };

            let c = &mut self.changes;
            match Pin::new(&mut *c).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    return Poll::Pending;
                }
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
                        let fut = Connection::connect(connector, endpoint);
                        self.connecting = Some((k, Box::pin(fut)));
                        continue;
                    }
                    Change::Remove(k) => return Poll::Ready(Some(Ok(Change::Remove(k)))),
                },
            }
        }
    }
}

impl<K: Hash + Eq + Clone> Unpin for DynamicServiceStream<K> {}
