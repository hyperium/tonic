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

use std::{future::Future, net::SocketAddr, pin::Pin};

use super::{DnsResolver, ResolverOptions, Runtime, Sleep, TaskHandle};

#[cfg(feature = "dns")]
mod hickory_resolver;

/// A DNS resolver that uses tokio::net::lookup_host for resolution. It only
/// supports host lookups.
pub struct TokioDefaultDnsResolver {}

#[tonic::async_trait]
impl DnsResolver for TokioDefaultDnsResolver {
    async fn lookup_host_name(&self, name: &str) -> Result<Vec<std::net::IpAddr>, String> {
        let name_with_port = match name.parse::<std::net::IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, 0).to_string(),
            Err(_) => format!("{}:0", name),
        };
        let ips = tokio::net::lookup_host(name_with_port)
            .await
            .map_err(|err| err.to_string())?
            .map(|socket_addr| socket_addr.ip())
            .collect();
        Ok(ips)
    }

    async fn lookup_txt(&self, _name: &str) -> Result<Vec<String>, String> {
        Err("TXT record lookup unavailable. Enable the optional 'dns' feature to enable service config lookups.".to_string())
    }
}

pub struct TokioRuntime {}

impl TaskHandle for tokio::task::JoinHandle<()> {
    fn abort(&self) {
        self.abort()
    }
}

impl Sleep for tokio::time::Sleep {}

impl Runtime for TokioRuntime {
    fn spawn(
        &self,
        task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    ) -> Box<dyn TaskHandle> {
        Box::new(tokio::spawn(task))
    }

    fn get_dns_resolver(&self, opts: ResolverOptions) -> Result<Box<dyn DnsResolver>, String> {
        #[cfg(feature = "dns")]
        {
            Ok(Box::new(hickory_resolver::DnsResolver::new(opts)?))
        }
        #[cfg(not(feature = "dns"))]
        {
            Ok(Box::new(TokioDefaultDnsResolver::new(opts)?))
        }
    }

    fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn Sleep>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

impl TokioDefaultDnsResolver {
    pub fn new(opts: ResolverOptions) -> Result<Self, String> {
        if opts.server_addr.is_some() {
            return Err("Custom DNS server are not supported, enable optional feature 'dns' to enable support.".to_string());
        }
        Ok(TokioDefaultDnsResolver {})
    }
}

#[cfg(test)]
mod tests {
    use super::{DnsResolver, ResolverOptions, Runtime, TokioDefaultDnsResolver, TokioRuntime};

    #[tokio::test]
    async fn lookup_hostname() {
        let runtime = TokioRuntime {};

        let dns = runtime
            .get_dns_resolver(ResolverOptions::default())
            .unwrap();
        let ips = dns.lookup_host_name("localhost").await.unwrap();
        assert!(
            !ips.is_empty(),
            "Expect localhost to resolve to more than 1 IPs."
        )
    }

    #[tokio::test]
    async fn default_resolver_txt_fails() {
        let default_resolver = TokioDefaultDnsResolver::new(ResolverOptions::default()).unwrap();

        let txt = default_resolver.lookup_txt("google.com").await;
        assert!(txt.is_err())
    }

    #[tokio::test]
    async fn default_resolver_custom_authority() {
        let opts = ResolverOptions {
            server_addr: Some("8.8.8.8:53".parse().unwrap()),
        };
        let default_resolver = TokioDefaultDnsResolver::new(opts);
        assert!(default_resolver.is_err())
    }
}
