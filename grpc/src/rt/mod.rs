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

use std::{future::Future, pin::Pin};

pub mod tokio;

/// An abstraction over an asynchronous runtime.
///
/// The `Runtime` trait defines the core functionality required for
/// executing asynchronous tasks, creating DNS resolvers, and performing
/// time-based operations such as sleeping. It provides a uniform interface
/// that can be implemented for various async runtimes, enabling pluggable
/// and testable infrastructure.
pub(super) trait Runtime: Send + Sync {
    /// Spawns the given asynchronous task to run in the background.
    fn spawn(
        &self,
        task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    ) -> Box<dyn TaskHandle>;

    /// Creates and returns an instance of a DNSResolver, optionally
    /// configured by the ResolverOptions struct. This method may return an
    /// error if it fails to create the DNSResolver.
    fn get_dns_resolver(&self, opts: ResolverOptions) -> Result<Box<dyn DnsResolver>, String>;

    /// Returns a future that completes after the specified duration.
    fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn Sleep>>;
}

/// A future that resolves after a specified duration.
pub(super) trait Sleep: Send + Sync + Future<Output = ()> {}

pub(super) trait TaskHandle: Send + Sync {
    /// Abort the associated task.
    fn abort(&self);
}

/// A trait for asynchronous DNS resolution.
#[tonic::async_trait]
pub(super) trait DnsResolver: Send + Sync {
    /// Resolve an address
    async fn lookup_host_name(&self, name: &str) -> Result<Vec<std::net::IpAddr>, String>;
    /// Perform a TXT record lookup. If a txt record contains multiple strings,
    /// they are concatenated.
    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String>;
}

#[derive(Default)]
pub(super) struct ResolverOptions {
    /// The address of the DNS server in "IP:port" format. If None, the
    /// system's default DNS server will be used.
    pub(super) server_addr: Option<std::net::SocketAddr>,
}
