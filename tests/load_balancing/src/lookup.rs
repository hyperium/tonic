use crate::pb::pong::Payload;
use crate::pb::tester_server::Tester;
use crate::pb::tester_server::TesterServer;
use crate::pb::{Ping, Pong};
use crate::TestServer;
use examples::load_balanced_channel::{LookupService, ServiceDefinition};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tonic::transport::ServerTlsConfig;
use tonic::Status;

#[derive(Clone)]
pub struct TesterImpl {
    pub sender: Arc<Mutex<tokio::sync::mpsc::Sender<String>>>,
    pub name: String,
}

#[async_trait::async_trait]
impl Tester for TesterImpl {
    async fn test(&self, _req: tonic::Request<Ping>) -> Result<tonic::Response<Pong>, Status> {
        {
            self.sender
                .lock()
                .await
                .send(self.name.clone())
                .await
                .unwrap();
        }
        Ok(tonic::Response::new(Pong {
            payload: Some(Payload::Raw(String::from(self.name.clone()))),
        }))
    }
}

#[derive(Clone)]
pub struct TestDnsResolver {
    pub ips: Arc<RwLock<HashMap<String, String>>>,
    pub servers: Arc<RwLock<HashMap<String, TestServer>>>,
    pub tls_config: Option<ServerTlsConfig>,
}

impl TestDnsResolver {
    pub fn new_with_tls(tls: ServerTlsConfig) -> Self {
        Self {
            ips: Arc::new(RwLock::new(HashMap::new())),
            servers: Arc::new(RwLock::new(HashMap::new())),
            tls_config: Some(tls),
        }
    }
}

impl Default for TestDnsResolver {
    fn default() -> Self {
        Self {
            ips: Arc::new(RwLock::new(HashMap::new())),
            servers: Arc::new(RwLock::new(HashMap::new())),
            tls_config: None,
        }
    }
}

impl TestDnsResolver {
    pub async fn remove_server(&mut self, server: String) {
        let mut ips = self.ips.write().await;
        ips.remove(&server).expect("server is not in the hashmap");
        let mut servers = self.servers.write().await;
        let server = servers
            .remove(&server)
            .expect("server is not in the hashmap");
        server.shutdown_sync().await;
    }

    pub async fn add_server_with_provided_impl(&mut self, name: String, server: impl Tester) {
        let mut ips = self.ips.write().await;
        let mut servers = self.servers.write().await;
        let test_server =
            TestServer::start(TesterServer::new(server), None, self.tls_config.clone()).await;

        tracing::debug!(
            "Adding server with name {} and address {}",
            name,
            test_server.address()
        );
        (*ips).insert(name.to_string(), test_server.address().to_string());
        (*servers).insert(name, test_server);
    }

    pub async fn add_ip_without_server(&mut self, name: String, ip: String) {
        let mut ips = self.ips.write().await;
        (*ips).insert(name.to_string(), ip);
    }

    pub async fn remove_ip_and_not_server(&mut self, name: String) {
        let mut ips = self.ips.write().await;
        ips.remove(&name)
            .expect("No IP registered against that name.");
    }
}

#[async_trait::async_trait]
impl LookupService for TestDnsResolver {
    async fn resolve_service_endpoints(
        &self,
        _definition: &ServiceDefinition,
    ) -> Result<HashSet<SocketAddr>, anyhow::Error> {
        let ips = self.ips.read().await;

        Ok(ips
            .values()
            .cloned()
            .into_iter()
            .map(|address| address.parse().expect("not a valid ip address"))
            .collect())
    }
}
