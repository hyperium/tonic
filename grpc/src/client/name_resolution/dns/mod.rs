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

//! This module implements a DNS resolver to be installed as the default resolver
//! in grpc.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

use tokio::sync::mpsc::UnboundedSender;
use url::Host;

use crate::{
    client::name_resolution::{Address, NopResolver, ResolverUpdate, TCP_IP_NETWORK_TYPE},
    rt,
};

use super::{
    backoff::{BackoffConfig, ExponentialBackoff, DEFAULT_EXPONENTIAL_CONFIG},
    global_registry, Endpoint, Resolver, ResolverBuilder,
};

#[cfg(test)]
mod test;

const DEFAULT_PORT: u16 = 443;
const DEFAULT_DNS_PORT: u16 = 53;

/// This specifies the maximum duration for a DNS resolution request.
/// If the timeout expires before a response is received, the request will be
/// canceled.
///
/// It is recommended to set this value at application startup. Avoid modifying
/// this variable after initialization.
static RESOLVING_TIMEOUT_MS: AtomicU64 = AtomicU64::new(30_000); // 30 seconds

/// This is the minimum interval at which re-resolutions are allowed. This helps
/// to prevent excessive re-resolution.
static MIN_RESOLUTION_INTERVAL_MS: AtomicU64 = AtomicU64::new(30_000); // 30 seconds

pub fn get_resolving_timeout() -> Duration {
    Duration::from_millis(RESOLVING_TIMEOUT_MS.load(Ordering::Relaxed))
}

pub fn set_resolving_timeout(duration: Duration) {
    RESOLVING_TIMEOUT_MS.store(duration.as_millis() as u64, Ordering::Relaxed);
}

pub fn get_min_resolution_interval() -> Duration {
    Duration::from_millis(MIN_RESOLUTION_INTERVAL_MS.load(Ordering::Relaxed))
}

pub fn set_min_resolution_interval(duration: Duration) {
    MIN_RESOLUTION_INTERVAL_MS.store(duration.as_millis() as u64, Ordering::Relaxed);
}

pub fn reg() {
    global_registry().add_builder(Box::new(Builder {}));
}

struct Builder {}

struct DnsOptions {
    min_resolution_interval: Duration,
    resolving_timeout: Duration,
    backoff_config: BackoffConfig,
}

impl DnsResolver {
    fn new(
        target: &super::Target,
        options: super::ResolverOptions,
        dns_opts: DnsOptions,
    ) -> Box<dyn Resolver + 'static> {
        let parsed = match parse_endpoint_and_authority(target) {
            Ok(res) => res,
            Err(err) => return nop_resolver_for_err(err.to_string(), options),
        };
        let endpoint = parsed.endpoint;
        let host = match endpoint.host {
            Host::Domain(d) => d,
            Host::Ipv4(ipv4) => {
                return nop_resolver_for_ip(IpAddr::V4(ipv4), endpoint.port, options)
            }
            Host::Ipv6(ipv6) => {
                return nop_resolver_for_ip(IpAddr::V6(ipv6), endpoint.port, options)
            }
        };
        let authority = parsed.authority;
        let dns = match options.runtime.get_dns_resolver(rt::ResolverOptions {
            server_addr: authority,
        }) {
            Ok(dns) => dns,
            Err(err) => return nop_resolver_for_err(err.to_string(), options),
        };
        let state = Arc::new(Mutex::new(InternalState {
            addrs: Ok(Vec::new()),
        }));
        let state_copy = state.clone();
        let (resolve_now_tx, mut resolve_now_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (update_error_tx, update_error_rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<(), String>>();

        let handle = options.runtime.clone().spawn(Box::pin(async move {
            let backoff = ExponentialBackoff::new(dns_opts.backoff_config.clone())
                .expect("default exponential config must be valid");
            let state = state_copy;
            let work_scheduler = options.work_scheduler;
            let mut update_error_rx = update_error_rx;
            loop {
                let mut lookup_fut = dns.lookup_host_name(&host);
                let mut timeout_fut = options.runtime.sleep(dns_opts.resolving_timeout);
                let addrs = tokio::select! {
                    result = &mut lookup_fut => {
                        match result {
                            Ok(ips) => {
                                let addrs = ips
                                    .into_iter()
                                    .map(|ip| SocketAddr::new(ip, endpoint.port))
                                    .collect();
                                Ok(addrs)
                            }
                            Err(err) => Err(err),
                        }
                    }
                    _ = &mut timeout_fut => {
                        Err("Timed out waiting for DNS resolution".to_string())
                    }
                };
                {
                    let mut internal_state = match state.lock() {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                    internal_state.addrs = addrs;
                }
                work_scheduler.schedule_work();
                let update_result = match update_error_rx.recv().await {
                    Some(res) => res,
                    None => return,
                };
                let next_resoltion_time: SystemTime;
                if update_result.is_err() {
                    next_resoltion_time = SystemTime::now()
                        .checked_add(backoff.backoff_duration())
                        .unwrap();
                } else {
                    // Success resolving, wait for the next resolve_now. However,
                    // also wait MIN_RESOLUTION_INTERVAL at the very least to prevent
                    // constantly re-resolving.
                    backoff.reset();
                    next_resoltion_time = SystemTime::now()
                        .checked_add(dns_opts.min_resolution_interval)
                        .unwrap();
                    _ = resolve_now_rx.recv().await;
                }
                // Wait till next resolution time.
                let duration = match next_resoltion_time.duration_since(SystemTime::now()) {
                    Ok(d) => d,
                    Err(_) => continue, // Time has already passed.
                };
                options.runtime.sleep(duration).await;
            }
        }));

        Box::new(DnsResolver {
            state,
            task_handle: handle,
            resolve_now_requester: resolve_now_tx,
            update_error_sender: update_error_tx,
        })
    }
}

impl ResolverBuilder for Builder {
    fn build(
        &self,
        target: &super::Target,
        options: super::ResolverOptions,
    ) -> Box<dyn super::Resolver> {
        let dns_opts = DnsOptions {
            min_resolution_interval: get_min_resolution_interval(),
            resolving_timeout: get_resolving_timeout(),
            backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
        };
        DnsResolver::new(target, options, dns_opts)
    }

    fn scheme(&self) -> &'static str {
        "dns"
    }

    fn is_valid_uri(&self, target: &super::Target) -> bool {
        if let Err(err) = parse_endpoint_and_authority(target) {
            eprintln!("{}", err);
            false
        } else {
            true
        }
    }
}

struct DnsResolver {
    state: Arc<Mutex<InternalState>>,
    task_handle: Box<dyn rt::TaskHandle>,
    resolve_now_requester: UnboundedSender<()>,
    update_error_sender: UnboundedSender<Result<(), String>>,
}

struct InternalState {
    addrs: Result<Vec<SocketAddr>, String>,
}

impl Resolver for DnsResolver {
    fn resolve_now(&mut self) {
        _ = self.resolve_now_requester.send(());
    }

    fn work(&mut self, channel_controller: &mut dyn super::ChannelController) {
        let state = match self.state.lock() {
            Err(_) => {
                eprintln!("DNS resolver mutex poisoned, can't update channel");
                return;
            }
            Ok(s) => s,
        };
        let endpoint_result = match &state.addrs {
            Ok(addrs) => {
                let endpoints: Vec<_> = addrs
                    .iter()
                    .map(|a| Endpoint {
                        addresses: vec![Address {
                            network_type: TCP_IP_NETWORK_TYPE,
                            address: a.to_string(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    })
                    .collect();
                Ok(endpoints)
            }
            Err(err) => Err(err.to_string()),
        };
        let update = ResolverUpdate {
            endpoints: endpoint_result,
            ..Default::default()
        };
        let status = channel_controller.update(update);
        _ = self.update_error_sender.send(status);
    }
}

impl Drop for DnsResolver {
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

#[derive(Eq, PartialEq, Debug)]
struct HostPort {
    host: Host<String>,
    port: u16,
}

#[derive(Eq, PartialEq, Debug)]
struct ParseResult {
    endpoint: HostPort,
    authority: Option<SocketAddr>,
}

fn parse_endpoint_and_authority(target: &super::Target) -> Result<ParseResult, String> {
    // Parse the endpoint.
    let endpoint = target.path();
    let endpoint = endpoint.strip_prefix("/").unwrap_or(endpoint);
    let parse_result = parse_host_port(endpoint, DEFAULT_PORT)
        .map_err(|err| format!("Failed to parse target {}: {}", target, err))?;
    let endpoint = parse_result.ok_or("Received empty endpoint host.".to_string())?;

    // Parse the authority.
    let authority = target.authority_host_port();
    if authority.is_empty() {
        return Ok(ParseResult {
            endpoint,
            authority: None,
        });
    }
    let parse_result = parse_host_port(&authority, DEFAULT_DNS_PORT)
        .map_err(|err| format!("Failed to parse DNS authority {}: {}", target, err))?;
    let Some(authority) = parse_result else {
        return Ok(ParseResult {
            endpoint,
            authority: None,
        });
    };
    let authority = match authority.host {
        Host::Ipv4(ipv4) => SocketAddr::new(IpAddr::V4(ipv4), authority.port),
        Host::Ipv6(ipv6) => SocketAddr::new(IpAddr::V6(ipv6), authority.port),
        _ => {
            return Err(format!("Received non-IP DNS authority {}", authority.host));
        }
    };
    Ok(ParseResult {
        endpoint,
        authority: Some(authority),
    })
}

/// Takes the user input string of the format "host:port" and default port,
/// returns the parsed host and port. If string doesn't specify a port, the
/// default_port is returned. If the string doesn't specify the host,
/// Result<None> is returned.
fn parse_host_port(host_and_port: &str, default_port: u16) -> Result<Option<HostPort>, String> {
    // We need to use the https scheme otherwise url::Url::parse doesn't convert
    // IP addresses to Host::Ipv4 or Host::Ipv6 if they could represent valid
    // domains.
    let url = format!("https://{}", host_and_port);
    let url = url.parse::<url::Url>().map_err(|err| err.to_string())?;
    let port = url.port().unwrap_or(default_port);
    let host = match url.host() {
        Some(host) => host,
        None => return Ok(None),
    };
    // Convert the domain to an owned string.
    let host = match host {
        Host::Domain(s) => Host::Domain(s.to_owned()),
        Host::Ipv4(ip) => Host::Ipv4(ip),
        Host::Ipv6(ip) => Host::Ipv6(ip),
    };
    Ok(Some(HostPort { host, port }))
}

fn nop_resolver_for_ip(
    ip: IpAddr,
    port: u16,
    options: super::ResolverOptions,
) -> Box<dyn super::Resolver> {
    options.work_scheduler.schedule_work();
    Box::new(NopResolver {
        update: ResolverUpdate {
            endpoints: Ok(vec![Endpoint {
                addresses: vec![Address {
                    network_type: TCP_IP_NETWORK_TYPE,
                    address: SocketAddr::new(ip, port).to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }]),
            ..Default::default()
        },
    })
}

fn nop_resolver_for_err(err: String, options: super::ResolverOptions) -> Box<dyn super::Resolver> {
    options.work_scheduler.schedule_work();
    Box::new(NopResolver {
        update: ResolverUpdate {
            endpoints: Err(err),
            ..Default::default()
        },
    })
}
