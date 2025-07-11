use std::sync::Arc;

use tokio::sync::oneshot;
use tonic::async_trait;

use crate::service::{Request, Response, Service};

pub struct Server {
    handler: Option<Arc<dyn Service>>,
}

pub type Call = (String, Request, oneshot::Sender<Response>);

#[async_trait]
pub trait Listener {
    async fn accept(&self) -> Option<Call>;
}

impl Server {
    pub fn new() -> Self {
        Self { handler: None }
    }

    pub fn set_handler(&mut self, f: impl Service + 'static) {
        self.handler = Some(Arc::new(f))
    }

    pub async fn serve(&self, l: &impl Listener) {
        while let Some((method, req, reply_on)) = l.accept().await {
            reply_on
                .send(self.handler.as_ref().unwrap().call(method, req).await)
                .ok(); // TODO: log error
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}
