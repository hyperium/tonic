use super::connection::Connection;
use crate::transport::Endpoint;
use crate::transport::EndpointManager;
use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::discover::{Change, Discover};

pub(crate) struct ServiceList {
    list: VecDeque<Endpoint>,
    connecting:
        Option<Pin<Box<dyn Future<Output = Result<Connection, crate::Error>> + Send + 'static>>>,
    i: usize,
}

pub(crate) struct DynamicServiceList {
    manager: Box<dyn EndpointManager>,
    connecting:
        Option<(usize, Pin<Box<dyn Future<Output = Result<Connection, crate::Error>> + Send + 'static>>)>,
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


impl DynamicServiceList {
    pub(crate) fn new(manager:Box<dyn EndpointManager>) -> Self {
        Self {
            manager,
            connecting: None,
        }
    }
}


impl Discover for DynamicServiceList {
    type Key = usize;
    type Service = Connection;
    type Error = crate::Error;

    fn poll_discover(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Change<Self::Key, Self::Service>, Self::Error>> {
        loop {
	    let waker =  cx.waker().clone();
            if let Some((key,connecting)) = &mut self.connecting {
                let svc = futures_core::ready!(Pin::new(connecting).poll(cx))?;
		let key = key.clone();
                self.connecting = None;
                let change = Ok(Change::Insert(key, svc));
                return Poll::Ready(change);
            };
	    
	    if let Some(key) = self.manager.to_remove(){
		let change = Ok(Change::Remove(key));
                return Poll::Ready(change);                
            };
	    
            if let Some((key,endpoint)) = self.manager.to_add() {
                let mut http = hyper::client::connect::HttpConnector::new();
                http.set_nodelay(endpoint.tcp_nodelay);
                http.set_keepalive(endpoint.tcp_keepalive);

                let fut = Connection::new(http, endpoint);
                self.connecting = Some((key,Box::pin(fut)));		
            } else {
		waker.wake();
                return Poll::Pending;
	    }

        }
    }
}
