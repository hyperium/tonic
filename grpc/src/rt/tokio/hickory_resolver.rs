/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};

/// A DNS resolver that uses hickory with the tokio runtime. This supports txt
/// lookups in addition to A and AAAA record lookups. It also supports using
/// custom DNS servers.
pub struct DnsResolver {
    resolver: hickory_resolver::TokioResolver,
}

#[tonic::async_trait]
impl super::DnsResolver for DnsResolver {
    async fn lookup_host_name(&self, name: &str) -> Result<Vec<std::net::IpAddr>, String> {
        let response = self
            .resolver
            .lookup_ip(name)
            .await
            .map_err(|err| err.to_string())?;
        Ok(response.iter().collect())
    }

    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String> {
        let response: Vec<_> = self
            .resolver
            .txt_lookup(name)
            .await
            .map_err(|err| err.to_string())?
            .iter()
            .map(|txt_record| {
                txt_record
                    .iter()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .collect::<Vec<String>>()
                    .join("")
            })
            .collect();
        Ok(response)
    }
}

impl DnsResolver {
    pub fn new(opts: super::ResolverOptions) -> Result<Self, String> {
        let builder = if let Some(server_addr) = opts.server_addr {
            let provider = hickory_resolver::name_server::TokioConnectionProvider::default();
            let name_servers = NameServerConfigGroup::from_ips_clear(
                &[server_addr.ip()],
                server_addr.port(),
                true,
            );
            let config = ResolverConfig::from_parts(None, vec![], name_servers);
            hickory_resolver::TokioResolver::builder_with_config(config, provider)
        } else {
            hickory_resolver::TokioResolver::builder_tokio().map_err(|err| err.to_string())?
        };
        let mut resolver_opts = ResolverOpts::default();
        resolver_opts.ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
        Ok(DnsResolver {
            resolver: builder.with_options(resolver_opts).build(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        sync::Arc,
    };

    use hickory_resolver::Name;
    use hickory_server::{
        authority::{Catalog, ZoneType},
        proto::rr::{
            rdata::{A, TXT},
            LowerName, RData, Record,
        },
        store::in_memory::InMemoryAuthority,
        ServerFuture,
    };
    use tokio::{net::UdpSocket, sync::oneshot, task::JoinHandle};

    use crate::rt::{tokio::TokioDefaultDnsResolver, DnsResolver, ResolverOptions};

    #[tokio::test]
    async fn compare_hickory_and_default() {
        let hickory_dns = super::DnsResolver::new(ResolverOptions::default()).unwrap();
        let mut ips_hickory = hickory_dns.lookup_host_name("localhost").await.unwrap();

        let default_resolver = TokioDefaultDnsResolver::new(ResolverOptions::default()).unwrap();

        let mut system_resolver_ips = default_resolver
            .lookup_host_name("localhost")
            .await
            .unwrap();

        // Hickory requests A and AAAA records in parallel, so the order of IPv4
        // and IPv6 addresses isn't deterministic.
        ips_hickory.sort();
        system_resolver_ips.sort();
        assert_eq!(
            ips_hickory, system_resolver_ips,
            "both resolvers should produce same IPs for localhost"
        )
    }

    #[tokio::test]
    async fn resolve_txt() {
        let records = vec![
            Record::from_rdata(
                Name::from_ascii("test.local.").unwrap(),
                300,
                RData::TXT(TXT::new(vec![
                    "one".to_string(),
                    "two".to_string(),
                    "three".to_string(),
                ])),
            ),
            Record::from_rdata(
                Name::from_ascii("test.local.").unwrap(),
                300,
                RData::TXT(TXT::new(vec![
                    "abc".to_string(),
                    "def".to_string(),
                    "ghi".to_string(),
                ])),
            ),
        ];

        let dns = start_in_memory_dns_server("test.local.", records).await;
        let opts = ResolverOptions {
            server_addr: Some(dns.addr),
        };
        let hickory_dns = super::DnsResolver::new(opts).unwrap();

        let txt = hickory_dns.lookup_txt("test.local").await.unwrap();
        assert_eq!(
            txt,
            vec!["onetwothree".to_string(), "abcdefghi".to_string(),]
        );
        dns.shutdown().await;
    }

    #[tokio::test]
    async fn custom_authority() {
        let record = Record::from_rdata(
            Name::from_ascii("test.local.").unwrap(),
            300,
            RData::A(A(Ipv4Addr::new(1, 2, 3, 4))),
        );
        let dns = start_in_memory_dns_server("test.local.", vec![record]).await;
        let opts = ResolverOptions {
            server_addr: Some(dns.addr),
        };
        let hickory_dns = super::DnsResolver::new(opts).unwrap();
        let ips = hickory_dns.lookup_host_name("test.local").await.unwrap();
        assert_eq!(ips, vec![Ipv4Addr::new(1, 2, 3, 4)]);
        dns.shutdown().await
    }

    struct FakeDns {
        tx: Option<oneshot::Sender<()>>,
        join_handle: Option<JoinHandle<()>>,
        addr: SocketAddr,
    }

    impl FakeDns {
        async fn shutdown(mut self) {
            let tx = self.tx.take().unwrap();
            tx.send(()).unwrap();
            let handle = self.join_handle.take().unwrap();
            handle.await.unwrap();
        }
    }

    /// Starts an in-memory DNS server with and adds the given records. Returns
    /// a DNS server which should be shutdown after the test. It uses a random
    /// port to bind since tests can run in parallel. The assigned port can be
    /// read from the returned struct.
    async fn start_in_memory_dns_server(host: &str, records: Vec<Record>) -> FakeDns {
        // Create a simple A record for `test.local.`
        let authority =
            InMemoryAuthority::empty(Name::from_ascii(host).unwrap(), ZoneType::Primary, false);

        for record in records {
            authority.upsert(record, 0).await;
        }

        let mut catalog = Catalog::new();
        catalog.upsert(
            LowerName::new(&Name::from_ascii(host).unwrap()),
            vec![Arc::new(authority)],
        );

        let mut server = ServerFuture::new(catalog);

        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        server.register_socket(socket);

        println!("DNS server running on {}", addr);

        let (tx, rx) = oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            tokio::select! {
                _ = server.block_until_done() => {},
                _ = rx => {
                    server.shutdown_gracefully().await.unwrap();
                }
            }
        });
        FakeDns {
            tx: Some(tx),
            join_handle: Some(server_task),
            addr,
        }
    }
}
