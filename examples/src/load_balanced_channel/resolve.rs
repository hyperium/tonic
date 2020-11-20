use crate::load_balanced_channel::ServiceDefinition;
use anyhow::Context;
use std::collections::HashSet;
use std::net::SocketAddr;
use trust_dns_resolver::{system_conf, AsyncResolver, TokioAsyncResolver};

/// Interface that provides functionality to
/// acquire a list of ips given a valid host name.
#[async_trait::async_trait]
pub trait LookupService {
    /// Return a list of unique `SockAddr` associated with the provided
    /// `ServiceDefinition` containing the `hostname` `port` of the service.
    /// If no ip addresses were resolved, an empty `HashSet` is returned.
    async fn resolve_service_endpoints(
        &self,
        definition: &ServiceDefinition,
    ) -> Result<HashSet<SocketAddr>, anyhow::Error>;
}

/// Implements `LookupService` by resolving the `hostname`
/// of the `ServiceDefinition` with a DNS lookup.
pub struct DnsResolver {
    /// The trust-dns resolver which contacts the dns service directly such
    /// that we bypass os-specific dns caching.
    dns: TokioAsyncResolver,
}

impl DnsResolver {
    /// Construct a new `DnsResolver` from env and system configration, e.g `resolv.conf`.
    pub async fn from_system_config() -> Result<Self, anyhow::Error> {
        let (config, mut opts) = system_conf::read_system_conf()
            .context("failed to read dns services from system configuration")?;

        // We do not want any caching on out side.
        opts.cache_size = 0;

        let dns = AsyncResolver::tokio(config, opts)
            .await
            .expect("resolver must be valid");

        Ok(Self { dns })
    }
}

#[async_trait::async_trait]
impl LookupService for DnsResolver {
    #[tracing::instrument(level = "debug", skip(self))]
    async fn resolve_service_endpoints(
        &self,
        definition: &ServiceDefinition,
    ) -> Result<HashSet<SocketAddr>, anyhow::Error> {
        match self.dns.lookup_ip(definition.hostname.as_ref()).await {
            Ok(lookup) => {
                tracing::debug!("dns query expires in: {:?}", lookup.valid_until());
                Ok(lookup
                    .iter()
                    .map(|ip_addr| {
                        tracing::debug!("result: ip {}", ip_addr);
                        (ip_addr, definition.port).into()
                    })
                    .collect())
            }
            Err(err) => Err(err.into()),
        }
    }
}
