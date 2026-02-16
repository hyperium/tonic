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

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use tokio::sync::mpsc::{self, UnboundedSender};
use url::Host;

use crate::{
    client::{
        name_resolution::{
            backoff::{BackoffConfig, DEFAULT_EXPONENTIAL_CONFIG},
            dns::{
                get_min_resolution_interval, get_resolving_timeout, parse_endpoint_and_authority,
                reg, DnsResolver, HostPort,
            },
            global_registry, ChannelController, Resolver, ResolverOptions, ResolverUpdate, Target,
            WorkScheduler,
        },
        service_config::ServiceConfig,
    },
    rt::{self, tokio::TokioRuntime, GrpcRuntime, TcpOptions},
};

use super::{DnsOptions, ParseResult};

const DEFAULT_TEST_SHORT_TIMEOUT: Duration = Duration::from_millis(10);

#[test]
pub(crate) fn target_parsing() {
    struct TestCase {
        input: &'static str,
        want_result: Result<ParseResult, String>,
    }
    let test_cases = vec![
        TestCase {
            input: "dns:///grpc.io",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: Host::Domain("grpc.io".to_string()),
                    port: 443,
                },
                authority: None,
            }),
        },
        TestCase {
            input: "dns:///grpc.io:1234",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: None,
            }),
        },
        TestCase {
            input: "dns://8.8.8.8/grpc.io:1234",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: Some("8.8.8.8:53".parse().unwrap()),
            }),
        },
        TestCase {
            input: "dns://8.8.8.8:5678/grpc.io:1234/abc",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: Some("8.8.8.8:5678".parse().unwrap()),
            }),
        },
        TestCase {
            input: "dns://[::1]:5678/grpc.io:1234/abc",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: Some("[::1]:5678".parse().unwrap()),
            }),
        },
        TestCase {
            input: "dns://[fe80::1]:5678/127.0.0.1:1234/abc",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: Host::Ipv4("127.0.0.1".parse().unwrap()),
                    port: 1234,
                },
                authority: Some("[fe80::1]:5678".parse().unwrap()),
            }),
        },
        TestCase {
            input: "dns:///[fe80::1%80]:5678/abc",
            want_result: Err("SocketAddr doesn't support IPv6 addresses with zones".to_string()),
        },
        TestCase {
            input: "dns:///:5678/abc",
            want_result: Err("Empty host with port".to_string()),
        },
        TestCase {
            input: "dns:///grpc.io:abc/abc",
            want_result: Err("Non numeric port".to_string()),
        },
        TestCase {
            input: "dns:///grpc.io:/",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: Host::Domain("grpc.io".to_string()),
                    port: 443,
                },
                authority: None,
            }),
        },
        TestCase {
            input: "dns:///:",
            want_result: Err("No host and port".to_string()),
        },
        TestCase {
            input: "dns:///[2001:db8:a0b:12f0::1",
            want_result: Err("Invalid address".to_string()),
        },
    ];

    for tc in test_cases {
        let target: Target = tc.input.parse().unwrap();
        let got = parse_endpoint_and_authority(&target);
        if got.is_err() != tc.want_result.is_err() {
            panic!(
                "Got error {:?}, want error: {:?}",
                got.err(),
                tc.want_result.err()
            );
        }
        if got.is_err() {
            continue;
        }
        assert_eq!(got.unwrap(), tc.want_result.unwrap());
    }
}

struct FakeWorkScheduler {
    work_tx: UnboundedSender<()>,
}

impl WorkScheduler for FakeWorkScheduler {
    fn schedule_work(&self) {
        self.work_tx.send(()).unwrap();
    }
}

struct FakeChannelController {
    update_result: Result<(), String>,
    update_tx: UnboundedSender<ResolverUpdate>,
}

impl ChannelController for FakeChannelController {
    fn update(&mut self, update: ResolverUpdate) -> Result<(), String> {
        println!("Received resolver update: {:?}", &update);
        self.update_tx.send(update).unwrap();
        self.update_result.clone()
    }

    fn parse_service_config(&self, _: &str) -> Result<ServiceConfig, String> {
        Err("Unimplemented".to_string())
    }
}

#[tokio::test]
pub(crate) async fn dns_basic() {
    reg();
    let builder = global_registry().get("dns").unwrap();
    let target = &"dns:///localhost:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(FakeWorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: rt::default_runtime(),
        work_scheduler: work_scheduler.clone(),
    };
    let mut resolver = builder.build(target, opts);

    // Wait for schedule work to be called.
    work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // A successful endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert!(update.endpoints.unwrap().len() > 1);
}

#[tokio::test]
pub(crate) async fn invalid_target() {
    reg();
    let builder = global_registry().get("dns").unwrap();
    let target = &"dns:///:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(FakeWorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: rt::default_runtime(),
        work_scheduler: work_scheduler.clone(),
    };
    let mut resolver = builder.build(target, opts);

    // Wait for schedule work to be called.
    work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // An error endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert!(update
        .endpoints
        .err()
        .unwrap()
        .contains(&target.to_string()));
}

#[derive(Clone, Debug)]
struct FakeDns {
    latency: Duration,
    lookup_result: Result<Vec<std::net::IpAddr>, String>,
}

#[tonic::async_trait]
impl rt::DnsResolver for FakeDns {
    async fn lookup_host_name(&self, _: &str) -> Result<Vec<std::net::IpAddr>, String> {
        tokio::time::sleep(self.latency).await;
        self.lookup_result.clone()
    }

    async fn lookup_txt(&self, _: &str) -> Result<Vec<String>, String> {
        Err("unimplemented".to_string())
    }
}

#[derive(Debug)]
struct FakeRuntime {
    inner: TokioRuntime,
    dns: FakeDns,
}

impl rt::Runtime for FakeRuntime {
    fn spawn(
        &self,
        task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    ) -> Box<dyn rt::TaskHandle> {
        self.inner.spawn(task)
    }

    fn get_dns_resolver(&self, _: rt::ResolverOptions) -> Result<Box<dyn rt::DnsResolver>, String> {
        Ok(Box::new(self.dns.clone()))
    }

    fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn rt::Sleep>> {
        self.inner.sleep(duration)
    }

    fn tcp_stream(
        &self,
        target: std::net::SocketAddr,
        opts: rt::TcpOptions,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn rt::GrpcEndpoint>, String>> + Send>> {
        self.inner.tcp_stream(target, opts)
    }

    fn listen_tcp(
        &self,
        _addr: std::net::SocketAddr,
        _opts: TcpOptions,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn rt::TcpListener>, String>> + Send>> {
        unimplemented!()
    }
}

#[tokio::test]
pub(crate) async fn dns_lookup_error() {
    reg();
    let builder = global_registry().get("dns").unwrap();
    let target = &"dns:///grpc.io:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(FakeWorkScheduler {
        work_tx: work_tx.clone(),
    });
    let runtime = FakeRuntime {
        inner: TokioRuntime::default(),
        dns: FakeDns {
            latency: Duration::from_secs(0),
            lookup_result: Err("test_error".to_string()),
        },
    };
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: GrpcRuntime::new(runtime),
        work_scheduler: work_scheduler.clone(),
    };
    let mut resolver = builder.build(target, opts);

    // Wait for schedule work to be called.
    work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // An error endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert!(update.endpoints.err().unwrap().contains("test_error"));
}

#[tokio::test]
pub(crate) async fn dns_lookup_timeout() {
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(FakeWorkScheduler {
        work_tx: work_tx.clone(),
    });
    let runtime = FakeRuntime {
        inner: TokioRuntime::default(),
        dns: FakeDns {
            latency: Duration::from_secs(20),
            lookup_result: Ok(Vec::new()),
        },
    };
    let dns_client = runtime.dns.clone();
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: GrpcRuntime::new(runtime),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_opts = DnsOptions {
        min_resolution_interval: get_min_resolution_interval(),
        resolving_timeout: DEFAULT_TEST_SHORT_TIMEOUT,
        backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
        host: "grpc.io".to_string(),
        port: 1234,
    };
    let mut resolver = DnsResolver::new(Box::new(dns_client), opts, dns_opts);

    // Wait for schedule work to be called.
    work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);

    // An error endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert!(update.endpoints.err().unwrap().contains("Timed out"));
}

#[tokio::test]
pub(crate) async fn rate_limit() {
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(FakeWorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: rt::default_runtime(),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_client = opts
        .runtime
        .get_dns_resolver(rt::ResolverOptions { server_addr: None })
        .unwrap();
    let dns_opts = DnsOptions {
        min_resolution_interval: Duration::from_secs(20),
        resolving_timeout: get_resolving_timeout(),
        backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
        host: "localhost".to_string(),
        port: 1234,
    };
    let mut resolver = DnsResolver::new(dns_client, opts, dns_opts);

    // Wait for schedule work to be called.
    work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // A successful endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert!(update.endpoints.unwrap().len() > 1);

    // Call resolve_now repeatedly, new updates should not be produced.
    for _ in 0..5 {
        resolver.resolve_now();
        tokio::select! {
            _ = work_rx.recv() => {
                panic!("Received unexpected work request from resolver");
            }
            _ = tokio::time::sleep(DEFAULT_TEST_SHORT_TIMEOUT) => {
                println!("No work requested from resolver.");
            }
        };
    }
}

#[tokio::test]
pub(crate) async fn re_resolution_after_success() {
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(FakeWorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: rt::default_runtime(),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_opts = DnsOptions {
        min_resolution_interval: Duration::from_millis(1),
        resolving_timeout: get_resolving_timeout(),
        backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
        host: "localhost".to_string(),
        port: 1234,
    };
    let dns_client = opts
        .runtime
        .get_dns_resolver(rt::ResolverOptions { server_addr: None })
        .unwrap();
    let mut resolver = DnsResolver::new(dns_client, opts, dns_opts);

    // Wait for schedule work to be called.
    work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // A successful endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert!(update.endpoints.unwrap().len() > 1);

    // Call resolve_now, a new update should be produced.
    resolver.resolve_now();
    work_rx.recv().await.unwrap();
    resolver.work(&mut channel_controller);
    let update = update_rx.recv().await.unwrap();
    assert!(update.endpoints.unwrap().len() > 1);
}

#[tokio::test]
pub(crate) async fn backoff_on_error() {
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(FakeWorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: rt::default_runtime(),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_opts = DnsOptions {
        min_resolution_interval: Duration::from_millis(1),
        resolving_timeout: get_resolving_timeout(),
        // Speed up the backoffs to make the test run faster.
        backoff_config: BackoffConfig {
            base_delay: Duration::from_millis(1),
            multiplier: 1.0,
            jitter: 0.0,
            max_delay: Duration::from_millis(1),
        },
        host: "localhost".to_string(),
        port: 1234,
    };
    let dns_client = opts
        .runtime
        .get_dns_resolver(rt::ResolverOptions { server_addr: None })
        .unwrap();

    let mut resolver = DnsResolver::new(dns_client, opts, dns_opts);

    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Err("test_error".to_string()),
    };

    // As the channel returned an error to the resolver, the resolver will
    // backoff and re-attempt resolution.
    for _ in 0..5 {
        work_rx.recv().await.unwrap();
        resolver.work(&mut channel_controller);
        let update = update_rx.recv().await.unwrap();
        assert!(update.endpoints.unwrap().len() > 1);
    }

    // This time the channel accepts the resolver update.
    channel_controller.update_result = Ok(());
    work_rx.recv().await.unwrap();
    resolver.work(&mut channel_controller);
    let update = update_rx.recv().await.unwrap();
    assert!(update.endpoints.unwrap().len() > 1);

    // Since the channel controller returns Ok(), the resolver will stop
    // producing more updates.
    tokio::select! {
        _ = work_rx.recv() => {
            panic!("Received unexpected work request from resolver.");
        }
        _ = tokio::time::sleep(DEFAULT_TEST_SHORT_TIMEOUT) => {
            println!("No event received from resolver.");
        }
    };
}
