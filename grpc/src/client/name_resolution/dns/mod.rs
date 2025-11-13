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

//! This module implements a DNS resolver to be installed as the default resolver
//! in grpc.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

use parking_lot::Mutex;
use tokio::sync::Notify;
use url::Host;

use crate::{
    byte_str::ByteStr,
    client::name_resolution::{global_registry, ChannelController, ResolverBuilder, Target},
    rt::{self, BoxedTaskHandle},
};

use super::{
    backoff::{BackoffConfig, ExponentialBackoff, DEFAULT_EXPONENTIAL_CONFIG},
    Address, Endpoint, NopResolver, Resolver, ResolverOptions, ResolverUpdate, TCP_IP_NETWORK_TYPE,
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

fn get_resolving_timeout() -> Duration {
    Duration::from_millis(RESOLVING_TIMEOUT_MS.load(Ordering::Relaxed))
}

/// Sets the maximum duration for DNS resolution requests.
///
/// This function affects the global timeout used by all channels using the DNS
/// name resolver scheme.
///
/// It must be called only at application startup, before any gRPC calls are
/// made.
///
/// The default value is 30 seconds. Setting the timeout too low may result in
/// premature timeouts during resolution, while setting it too high may lead to
/// unnecessary delays in service discovery. Choose a value appropriate for your
/// specific needs and network environment.
pub(crate) fn set_resolving_timeout(duration: Duration) {
    RESOLVING_TIMEOUT_MS.store(duration.as_millis() as u64, Ordering::Relaxed);
}

fn get_min_resolution_interval() -> Duration {
    Duration::from_millis(MIN_RESOLUTION_INTERVAL_MS.load(Ordering::Relaxed))
}

/// Sets the default minimum interval at which DNS re-resolutions are allowed.
/// This helps to prevent excessive re-resolution.
///
/// It must be called only at application startup, before any gRPC calls are
/// made.
pub(crate) fn set_min_resolution_interval(duration: Duration) {
    MIN_RESOLUTION_INTERVAL_MS.store(duration.as_millis() as u64, Ordering::Relaxed);
}

pub(crate) fn reg() {
    global_registry().add_builder(Box::new(Builder {}));
}

struct Builder {}

struct DnsOptions {
    min_resolution_interval: Duration,
    resolving_timeout: Duration,
    backoff_config: BackoffConfig,
    host: String,
    port: u16,
}

impl DnsResolver {
    fn new(
        dns_client: Box<dyn rt::DnsResolver>,
        options: ResolverOptions,
        dns_opts: DnsOptions,
    ) -> Self {
        let state = Arc::new(Mutex::new(InternalState {
            addrs: Ok(Vec::new()),
            channel_response: None,
        }));
        let state_copy = state.clone();
        let resolve_now_notify = Arc::new(Notify::new());
        let channel_updated_notify = Arc::new(Notify::new());
        let channel_updated_rx = channel_updated_notify.clone();
        let resolve_now_rx = resolve_now_notify.clone();

        let runtime = options.runtime.clone();
        let work_scheduler = options.work_scheduler.clone();
        let handle = options.runtime.spawn(Box::pin(async move {
            let mut backoff = ExponentialBackoff::new(dns_opts.backoff_config.clone())
                .expect("default exponential config must be valid");
            let state = state_copy;
            loop {
                let mut lookup_fut = dns_client.lookup_host_name(&dns_opts.host);
                let mut timeout_fut = runtime.sleep(dns_opts.resolving_timeout);
                let addrs = tokio::select! {
                    result = &mut lookup_fut => {
                        match result {
                            Ok(ips) => {
                                let addrs = ips
                                    .into_iter()
                                    .map(|ip| SocketAddr::new(ip, dns_opts.port))
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
                    state.lock().addrs = addrs;
                }
                work_scheduler.schedule_work();
                channel_updated_rx.notified().await;
                let channel_response = { state.lock().channel_response.take() };
                let next_resoltion_time = if channel_response.is_some() {
                    SystemTime::now()
                        .checked_add(backoff.backoff_duration())
                        .unwrap()
                } else {
                    // Success resolving, wait for the next resolve_now. However,
                    // also wait MIN_RESOLUTION_INTERVAL at the very least to prevent
                    // constantly re-resolving.
                    backoff.reset();
                    let res_time = SystemTime::now()
                        .checked_add(dns_opts.min_resolution_interval)
                        .unwrap();
                    _ = resolve_now_rx.notified().await;
                    res_time
                };
                // Wait till next resolution time.
                let Ok(duration) = next_resoltion_time.duration_since(SystemTime::now()) else {
                    continue; // Time has already passed.
                };
                runtime.sleep(duration).await;
            }
        }));

        Self {
            state,
            task_handle: handle,
            resolve_now_notifier: resolve_now_notify,
            channel_update_notifier: channel_updated_notify,
        }
    }
}

impl ResolverBuilder for Builder {
    fn build(&self, target: &Target, options: ResolverOptions) -> Box<dyn Resolver> {
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
        let dns_client = match options.runtime.get_dns_resolver(rt::ResolverOptions {
            server_addr: authority,
        }) {
            Ok(dns) => dns,
            Err(err) => return nop_resolver_for_err(err.to_string(), options),
        };
        let dns_opts = DnsOptions {
            min_resolution_interval: get_min_resolution_interval(),
            resolving_timeout: get_resolving_timeout(),
            backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
            host,
            port: endpoint.port,
        };
        Box::new(DnsResolver::new(dns_client, options, dns_opts))
    }

    fn scheme(&self) -> &'static str {
        "dns"
    }

    fn is_valid_uri(&self, target: &Target) -> bool {
        if let Err(err) = parse_endpoint_and_authority(target) {
            eprintln!("{err}");
            false
        } else {
            true
        }
    }
}

struct DnsResolver {
    state: Arc<Mutex<InternalState>>,
    task_handle: BoxedTaskHandle,
    resolve_now_notifier: Arc<Notify>,
    channel_update_notifier: Arc<Notify>,
}

struct InternalState {
    addrs: Result<Vec<SocketAddr>, String>,
    // Error from the latest call to channel_controller.update().
    channel_response: Option<String>,
}

impl Resolver for DnsResolver {
    fn resolve_now(&mut self) {
        self.resolve_now_notifier.notify_one();
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        let mut state = self.state.lock();
        let endpoint_result = match &state.addrs {
            Ok(addrs) => {
                let endpoints: Vec<_> = addrs
                    .iter()
                    .map(|a| Endpoint {
                        addresses: vec![Address {
                            network_type: TCP_IP_NETWORK_TYPE,
                            address: ByteStr::from(a.to_string()),
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
        state.channel_response = status.err();
        self.channel_update_notifier.notify_one();
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

fn parse_endpoint_and_authority(target: &Target) -> Result<ParseResult, String> {
    // Parse the endpoint.
    let endpoint = target.path();
    let endpoint = endpoint.strip_prefix("/").unwrap_or(endpoint);
    let parse_result = parse_host_port(endpoint, DEFAULT_PORT)
        .map_err(|err| format!("Failed to parse target {target}: {err}"))?;
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
        .map_err(|err| format!("Failed to parse DNS authority {target}: {err}"))?;
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
/// Ok(None) is returned.
fn parse_host_port(host_and_port: &str, default_port: u16) -> Result<Option<HostPort>, String> {
    // We need to use the https scheme otherwise url::Url::parse doesn't convert
    // IP addresses to Host::Ipv4 or Host::Ipv6 if they could represent valid
    // domains.
    let url = format!("https://{host_and_port}");
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

fn nop_resolver_for_ip(ip: IpAddr, port: u16, options: ResolverOptions) -> Box<dyn Resolver> {
    options.work_scheduler.schedule_work();
    Box::new(NopResolver {
        update: ResolverUpdate {
            endpoints: Ok(vec![Endpoint {
                addresses: vec![Address {
                    network_type: TCP_IP_NETWORK_TYPE,
                    address: ByteStr::from(SocketAddr::new(ip, port).to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }]),
            ..Default::default()
        },
    })
}

fn nop_resolver_for_err(err: String, options: ResolverOptions) -> Box<dyn Resolver> {
    options.work_scheduler.schedule_work();
    Box::new(NopResolver {
        update: ResolverUpdate {
            endpoints: Err(err),
            ..Default::default()
        },
    })
}
