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

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{
    client::name_resolution::{
        self,
        backoff::{BackoffConfig, DEFAULT_EXPONENTIAL_CONFIG},
        dns::{parse_endpoint_and_authority, HostPort},
        ResolverOptions, ResolverUpdate, Target, GLOBAL_RESOLVER_REGISTRY,
    },
    rt::{self, tokio::TokioRuntime},
};

use super::ParseResult;

const DEFAULT_TEST_SHORT_TIMEOUT: Duration = Duration::from_millis(10);

#[test]
pub fn target_parsing() {
    struct TestCase {
        input: &'static str,
        want_result: Result<ParseResult, String>,
    }
    let test_cases = vec![
        TestCase {
            input: "dns:///grpc.io",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: url::Host::Domain("grpc.io".to_string()),
                    port: 443,
                },
                authority: None,
            }),
        },
        TestCase {
            input: "dns:///grpc.io:1234",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: url::Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: None,
            }),
        },
        TestCase {
            input: "dns://8.8.8.8/grpc.io:1234",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: url::Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: Some("8.8.8.8:53".parse().unwrap()),
            }),
        },
        TestCase {
            input: "dns://8.8.8.8:5678/grpc.io:1234/abc",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: url::Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: Some("8.8.8.8:5678".parse().unwrap()),
            }),
        },
        TestCase {
            input: "dns://[::1]:5678/grpc.io:1234/abc",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: url::Host::Domain("grpc.io".to_string()),
                    port: 1234,
                },
                authority: Some("[::1]:5678".parse().unwrap()),
            }),
        },
        TestCase {
            input: "dns://[fe80::1]:5678/127.0.0.1:1234/abc",
            want_result: Ok(ParseResult {
                endpoint: HostPort {
                    host: url::Host::Ipv4("127.0.0.1".parse().unwrap()),
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
                    host: url::Host::Domain("grpc.io".to_string()),
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

struct WorkScheduler {
    work_tx: UnboundedSender<()>,
}

impl name_resolution::WorkScheduler for WorkScheduler {
    fn schedule_work(&self) {
        self.work_tx.send(()).unwrap();
    }
}

struct FakeChannelController {
    update_result: Result<(), String>,
    update_tx: UnboundedSender<ResolverUpdate>,
}

impl name_resolution::ChannelController for FakeChannelController {
    fn update(&mut self, update: name_resolution::ResolverUpdate) -> Result<(), String> {
        println!("Received resolver update: {:?}", &update);
        self.update_tx.send(update).unwrap();
        self.update_result.clone()
    }

    fn parse_service_config(
        &self,
        _: &str,
    ) -> Result<crate::client::service_config::ServiceConfig, String> {
        Err("Unimplemented".to_string())
    }
}

#[tokio::test]
pub async fn dns_basic() {
    super::reg();
    let builder = GLOBAL_RESOLVER_REGISTRY.get("dns").unwrap();
    let target = &"dns:///localhost:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(WorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: Arc::new(TokioRuntime {}),
        work_scheduler: work_scheduler.clone(),
    };
    let mut resolver = builder.build(target, opts);

    // Wait for schedule work to be called.
    let _ = work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // A successful endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert_eq!(update.endpoints.unwrap().len() > 1, true);
}

#[tokio::test]
pub async fn invalid_target() {
    super::reg();
    let builder = GLOBAL_RESOLVER_REGISTRY.get("dns").unwrap();
    let target = &"dns:///:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(WorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: Arc::new(TokioRuntime {}),
        work_scheduler: work_scheduler.clone(),
    };
    let mut resolver = builder.build(target, opts);

    // Wait for schedule work to be called.
    let _ = work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // An error endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert_eq!(
        update
            .endpoints
            .err()
            .unwrap()
            .contains(&target.to_string()),
        true
    );
}

#[derive(Clone)]
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
}

#[tokio::test]
pub async fn dns_lookup_error() {
    super::reg();
    let builder = GLOBAL_RESOLVER_REGISTRY.get("dns").unwrap();
    let target = &"dns:///grpc.io:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(WorkScheduler {
        work_tx: work_tx.clone(),
    });
    let runtime = FakeRuntime {
        inner: TokioRuntime {},
        dns: FakeDns {
            latency: Duration::from_secs(0),
            lookup_result: Err("test_error".to_string()),
        },
    };
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: Arc::new(runtime),
        work_scheduler: work_scheduler.clone(),
    };
    let mut resolver = builder.build(target, opts);

    // Wait for schedule work to be called.
    let _ = work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // An error endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert_eq!(update.endpoints.err().unwrap().contains("test_error"), true);
}

#[tokio::test]
pub async fn dns_lookup_timeout() {
    let target = &"dns:///grpc.io:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(WorkScheduler {
        work_tx: work_tx.clone(),
    });
    let runtime = FakeRuntime {
        inner: TokioRuntime {},
        dns: FakeDns {
            latency: Duration::from_secs(20),
            lookup_result: Ok(Vec::new()),
        },
    };
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: Arc::new(runtime),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_opts = super::DnsOptions {
        min_resolution_interval: super::get_min_resolution_interval(),
        resolving_timeout: DEFAULT_TEST_SHORT_TIMEOUT,
        backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
    };
    let mut resolver = super::DnsResolver::new(target, opts, dns_opts);

    // Wait for schedule work to be called.
    let _ = work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);

    // An error endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert_eq!(update.endpoints.err().unwrap().contains("Timed out"), true);
}

#[tokio::test]
pub async fn rate_limit() {
    let target = &"dns:///localhost:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(WorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: Arc::new(TokioRuntime {}),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_opts = super::DnsOptions {
        min_resolution_interval: Duration::from_secs(20),
        resolving_timeout: super::get_resolving_timeout(),
        backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
    };
    let mut resolver = super::DnsResolver::new(target, opts, dns_opts);

    // Wait for schedule work to be called.
    let event = work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // A successful endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert_eq!(update.endpoints.unwrap().len() > 1, true);

    // Call resolve_now repeatedly, new updates should not be produced.
    for _ in 0..5 {
        resolver.resolve_now();
        tokio::select! {
            _ = work_rx.recv() => {
                panic!("Received unexpected work request from resolver: {:?}", event);
            }
            _ = tokio::time::sleep(DEFAULT_TEST_SHORT_TIMEOUT) => {
                println!("No work requested from resolver.");
            }
        };
    }
}

#[tokio::test]
pub async fn re_resolution_after_success() {
    let target = &"dns:///localhost:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(WorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: Arc::new(TokioRuntime {}),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_opts = super::DnsOptions {
        min_resolution_interval: Duration::from_millis(1),
        resolving_timeout: super::get_resolving_timeout(),
        backoff_config: DEFAULT_EXPONENTIAL_CONFIG,
    };
    let mut resolver = super::DnsResolver::new(target, opts, dns_opts);

    // Wait for schedule work to be called.
    let _ = work_rx.recv().await.unwrap();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Ok(()),
    };
    resolver.work(&mut channel_controller);
    // A successful endpoint update should be received.
    let update = update_rx.recv().await.unwrap();
    assert_eq!(update.endpoints.unwrap().len() > 1, true);

    // Call resolve_now, a new update should be produced.
    resolver.resolve_now();
    let _ = work_rx.recv().await.unwrap();
    resolver.work(&mut channel_controller);
    let update = update_rx.recv().await.unwrap();
    assert_eq!(update.endpoints.unwrap().len() > 1, true);
}

#[tokio::test]
pub async fn backoff_on_error() {
    let target = &"dns:///localhost:1234".parse().unwrap();
    let (work_tx, mut work_rx) = mpsc::unbounded_channel();
    let work_scheduler = Arc::new(WorkScheduler {
        work_tx: work_tx.clone(),
    });
    let opts = ResolverOptions {
        authority: "ignored".to_string(),
        runtime: Arc::new(TokioRuntime {}),
        work_scheduler: work_scheduler.clone(),
    };
    let dns_opts = super::DnsOptions {
        min_resolution_interval: Duration::from_millis(1),
        resolving_timeout: super::get_resolving_timeout(),
        // Speed up the backoffs to make the test run faster.
        backoff_config: BackoffConfig {
            base_delay: Duration::from_millis(1),
            multiplier: 1.0,
            jitter: 0.0,
            max_delay: Duration::from_millis(1),
        },
    };
    let mut resolver = super::DnsResolver::new(target, opts, dns_opts);

    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut channel_controller = FakeChannelController {
        update_tx,
        update_result: Err("test_error".to_string()),
    };

    // As the channel returned an error to the resolver, the resolver will
    // backoff and re-attempt resolution.
    for _ in 0..5 {
        let _ = work_rx.recv().await.unwrap();
        resolver.work(&mut channel_controller);
        let update = update_rx.recv().await.unwrap();
        assert_eq!(update.endpoints.unwrap().len() > 1, true);
    }

    // This time the channel accepts the resolver update.
    channel_controller.update_result = Ok(());
    let _ = work_rx.recv().await.unwrap();
    resolver.work(&mut channel_controller);
    let update = update_rx.recv().await.unwrap();
    assert_eq!(update.endpoints.unwrap().len() > 1, true);

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
