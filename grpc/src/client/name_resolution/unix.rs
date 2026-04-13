/*
 *
 * Copyright 2026 gRPC authors.
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

use crate::attributes::Attributes;
use crate::byte_str::ByteStr;
use crate::client::name_resolution::Address;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::NopResolver;
use crate::client::name_resolution::Resolver;
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::ResolverOptions;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::Target;
use crate::client::name_resolution::UNIX_NETWORK_TYPE;
use crate::client::name_resolution::global_registry;
use crate::client::name_resolution::nop_resolver_for_err;

#[derive(Debug, Copy, Clone)]
enum UnixScheme {
    Standard,
    #[cfg(target_os = "linux")]
    Abstract,
}

impl UnixScheme {
    const fn as_str(&self) -> &'static str {
        match self {
            UnixScheme::Standard => "unix",
            #[cfg(target_os = "linux")]
            UnixScheme::Abstract => "unix-abstract",
        }
    }
}

pub(crate) fn reg() {
    global_registry().add_builder(Box::new(Builder {
        scheme: UnixScheme::Standard,
    }));

    #[cfg(target_os = "linux")]
    global_registry().add_builder(Box::new(Builder {
        scheme: UnixScheme::Abstract,
    }));
}

#[derive(Debug)]
struct Builder {
    scheme: UnixScheme,
}

impl ResolverBuilder for Builder {
    fn build(
        &self,
        target: &super::Target,
        options: super::ResolverOptions,
    ) -> Box<dyn super::Resolver> {
        match parse_endpoint_and_authority(target, self.scheme) {
            Ok(addr) => nop_resolver_for_addr(addr, options),
            Err(err) => nop_resolver_for_err(err, options),
        }
    }

    fn scheme(&self) -> &str {
        self.scheme.as_str()
    }

    fn is_valid_uri(&self, uri: &super::Target) -> bool {
        parse_endpoint_and_authority(uri, self.scheme).is_ok()
    }
}

/// Parses a target URI into a standard or abstract UNIX domain socket address.
///
/// This function handles two schemes:
///
/// ### `unix` (Standard UNIX Domain Sockets)
/// Valid formats: `unix:path` or `unix:///absolute_path`
/// - `path` indicates the location of the desired socket on the filesystem.
/// - In the first form (`unix:path`), the path may be relative or absolute.
/// - In the second form (`unix:///absolute_path`), the path must be absolute.
///   The last of the three slashes is treated as the root of the filesystem
///   path (e.g., `/absolute_path`).
///
/// ### `unix-abstract` (Abstract Namespace)
/// Valid format: `unix-abstract:abstract_path`
/// - `abstract_path` indicates a socket name in the abstract namespace.
/// - The name has no connection with filesystem pathnames and bypasses standard
///   filesystem permissions; any process or user may access the socket.
/// - The underlying system requires a null byte (`\0`) as the first character.
///   This function automatically prepends the null byte; it should not be
///   included it in `abstract_path`.
/// - Note: Abstract sockets are a Linux-specific kernel feature.
fn parse_endpoint_and_authority(target: &Target, scheme: UnixScheme) -> Result<Address, String> {
    let host_port = target.authority_host_port();
    if !host_port.is_empty() {
        return Err(format!("invalid (non-empty) authority: {host_port}"));
    }
    let addr_string = match scheme {
        UnixScheme::Standard => target.path().to_owned(),
        #[cfg(target_os = "linux")]
        UnixScheme::Abstract => format!("\0{}", target.path()),
    };
    Ok(Address {
        network_type: UNIX_NETWORK_TYPE,
        address: ByteStr::from(addr_string),
        attributes: Attributes::new(),
    })
}

fn nop_resolver_for_addr(addr: Address, options: ResolverOptions) -> Box<dyn Resolver> {
    options.work_scheduler.schedule_work();
    Box::new(NopResolver {
        update: ResolverUpdate {
            endpoints: Ok(vec![Endpoint {
                addresses: vec![addr],
                ..Default::default()
            }]),
            ..Default::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::*;
    use crate::client::name_resolution::ChannelController;
    use crate::client::name_resolution::WorkScheduler;
    use crate::client::service_config::ServiceConfig;
    use crate::rt;

    struct FakeWorkScheduler {
        work_tx: mpsc::UnboundedSender<()>,
    }

    impl WorkScheduler for FakeWorkScheduler {
        fn schedule_work(&self) {
            self.work_tx.send(()).unwrap();
        }
    }

    struct FakeChannelController {
        update_tx: mpsc::UnboundedSender<ResolverUpdate>,
    }

    impl ChannelController for FakeChannelController {
        fn update(&mut self, update: ResolverUpdate) -> Result<(), String> {
            self.update_tx.send(update).unwrap();
            Ok(())
        }

        fn parse_service_config(&self, _: &str) -> Result<ServiceConfig, String> {
            Err("Unimplemented".to_string())
        }
    }

    #[tokio::test]
    async fn test_unix_resolver() {
        reg();

        struct TestCase {
            input: &'static str,
            scheme: &'static str,
            want_addr: &'static str,
            want_success: bool,
        }

        let test_cases = vec![
            TestCase {
                input: "unix:path/to/socket",
                scheme: "unix",
                want_addr: "path/to/socket",
                want_success: true,
            },
            TestCase {
                input: "unix:/absolute/path",
                scheme: "unix",
                want_addr: "/absolute/path",
                want_success: true,
            },
            TestCase {
                input: "unix:///absolute/path",
                scheme: "unix",
                want_addr: "/absolute/path",
                want_success: true,
            },
            #[cfg(target_os = "linux")]
            TestCase {
                input: "unix-abstract:abstract_name",
                scheme: "unix-abstract",
                want_addr: "\0abstract_name",
                want_success: true,
            },
            TestCase {
                input: "unix://authority/path",
                scheme: "unix",
                want_addr: "",
                want_success: false,
            },
        ];

        for tc in test_cases {
            let target: Target = tc.input.parse().expect("Failed to parse target");
            let (work_tx, mut work_rx) = mpsc::unbounded_channel();
            let work_scheduler = Arc::new(FakeWorkScheduler { work_tx });
            let opts = ResolverOptions {
                authority: "ignored".to_string(),
                runtime: rt::default_runtime(),
                work_scheduler: work_scheduler.clone(),
            };

            let builder = global_registry().get(tc.scheme).expect("scheme not found");
            let mut resolver = builder.build(&target, opts);

            // Wait for work to be scheduled.
            work_rx.recv().await.unwrap();

            let (update_tx, mut update_rx) = mpsc::unbounded_channel();
            let mut channel_controller = FakeChannelController { update_tx };
            resolver.work(&mut channel_controller);

            let update = update_rx.recv().await.unwrap();
            if tc.want_success {
                let endpoints = update.endpoints.expect("Should have succeeded");
                assert_eq!(endpoints.len(), 1);
                let addr = &endpoints[0].addresses[0];
                assert_eq!(addr.network_type, UNIX_NETWORK_TYPE);
                assert_eq!(&*addr.address, tc.want_addr);
            } else {
                let err = update.endpoints.expect_err("Should have failed");
                assert!(err.contains("invalid (non-empty) authority"));
            }
        }
    }
}
