/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::net::IpAddr;

use hickory_resolver::TokioResolver;
use hickory_resolver::config::ConnectionConfig;
use hickory_resolver::config::LookupIpStrategy;
use hickory_resolver::config::NameServerConfig;
use hickory_resolver::config::ResolverConfig;
use hickory_resolver::config::ResolverOpts;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;

use crate::rt::ResolverOptions;
use crate::rt::{self};

/// A DNS resolver that uses hickory with the tokio runtime. This supports txt
/// lookups in addition to A and AAAA record lookups. It also supports using
/// custom DNS servers.
pub(super) struct DnsResolver {
    resolver: hickory_resolver::TokioResolver,
}

#[tonic::async_trait]
impl rt::DnsResolver for DnsResolver {
    async fn lookup_host_name(&self, name: &str) -> Result<Vec<IpAddr>, String> {
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
            .answers()
            .iter()
            .filter_map(|record| {
                if let RData::TXT(txt) = &record.data {
                    Some(
                        txt.txt_data
                            .iter()
                            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                            .collect::<String>(),
                    )
                } else {
                    None
                }
            })
            .collect();
        Ok(response)
    }
}

impl DnsResolver {
    pub(super) fn new(opts: ResolverOptions) -> Result<Self, String> {
        let builder = if let Some(server_addr) = opts.server_addr {
            let mut udp = ConnectionConfig::udp();
            udp.port = server_addr.port();
            let mut tcp = ConnectionConfig::tcp();
            tcp.port = server_addr.port();
            let name_server = NameServerConfig::new(server_addr.ip(), true, vec![udp, tcp]);
            let config = ResolverConfig::from_parts(None, vec![], vec![name_server]);
            TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        } else {
            TokioResolver::builder_tokio().map_err(|err| err.to_string())?
        };
        let mut resolver_opts = ResolverOpts::default();
        resolver_opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        Ok(DnsResolver {
            resolver: builder
                .with_options(resolver_opts)
                .build()
                .map_err(|err| err.to_string())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use hickory_resolver::proto::rr::Name;
    use hickory_server::Server;
    use hickory_server::proto::rr::LowerName;
    use hickory_server::proto::rr::RData;
    use hickory_server::proto::rr::Record;
    use hickory_server::proto::rr::rdata::A;
    use hickory_server::proto::rr::rdata::TXT;
    use hickory_server::store::in_memory::InMemoryZoneHandler;
    use hickory_server::zone_handler::AxfrPolicy;
    use hickory_server::zone_handler::Catalog;
    use hickory_server::zone_handler::ZoneType;
    use tokio::net::UdpSocket;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use crate::rt::DnsResolver;
    use crate::rt::ResolverOptions;
    use crate::rt::tokio::TokioDefaultDnsResolver;

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
        let authority: InMemoryZoneHandler = InMemoryZoneHandler::empty(
            Name::from_ascii(host).unwrap(),
            ZoneType::Primary,
            AxfrPolicy::Deny,
        );

        for record in records {
            authority.upsert(record, 0).await;
        }

        let mut catalog = Catalog::new();
        catalog.upsert(
            LowerName::new(&Name::from_ascii(host).unwrap()),
            vec![Arc::new(authority)],
        );

        let mut server = Server::new(catalog);

        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        server.register_socket(socket);

        println!("DNS server running on {addr}");

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
