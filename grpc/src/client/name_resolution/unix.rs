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
use crate::client::name_resolution::NopResolver;
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::Target;
use crate::client::name_resolution::UNIX_NETWORK_TYPE;
use crate::client::name_resolution::global_registry;

pub(crate) fn reg() {
    global_registry().add_builder(Box::new(Builder {}));
}

#[derive(Debug)]
struct Builder {}

impl ResolverBuilder for Builder {
    fn build(
        &self,
        target: &super::Target,
        options: super::ResolverOptions,
    ) -> Box<dyn super::Resolver> {
        match parse_target(target) {
            Ok(addr) => NopResolver::new_with_addr(addr, options),
            Err(err) => NopResolver::new_with_err(err, options),
        }
    }

    fn scheme(&self) -> &str {
        "unix"
    }

    fn is_valid_uri(&self, uri: &super::Target) -> bool {
        parse_target(uri).is_ok()
    }

    fn default_authority(&self, target: &Target) -> String {
        "localhost".to_owned()
    }
}

/// Parses a target URI into a standard domain socket address.
///
/// Valid formats: `unix:path` or `unix:///absolute_path`
/// - `path` indicates the location of the desired socket on the filesystem.
/// - In the first form (`unix:path`), the path may be relative or absolute.
/// - In the second form (`unix:///absolute_path`), the path must be absolute.
///   The last of the three slashes is treated as the root of the filesystem
///   path (e.g., `/absolute_path`).
fn parse_target(target: &Target) -> Result<Address, String> {
    let host_port = target.authority_host_port();
    if !host_port.is_empty() {
        return Err(format!("invalid (non-empty) authority: {host_port}"));
    }
    let addr_string = target.path().to_owned();
    Ok(Address {
        network_type: UNIX_NETWORK_TYPE,
        address: ByteStr::from(addr_string),
        attributes: Attributes::new(),
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::client::name_resolution::ResolverOptions;
    use crate::client::name_resolution::test_utils::TestChannelController;
    use crate::client::name_resolution::test_utils::TestWorkScheduler;
    use crate::rt;

    #[rstest]
    #[case::relative_path("unix:path/to/socket", "path/to/socket")]
    #[case::absolute_path("unix:/absolute/path", "/absolute/path")]
    #[case::absolute_path_with_slashes("unix:///absolute/path", "/absolute/path")]
    #[tokio::test]
    async fn unix_resolver_success(#[case] input: &str, #[case] want_addr: &str) {
        reg();

        let target: Target = input.parse().expect("Failed to parse target");
        let (work_scheduler, mut work_rx) = TestWorkScheduler::new_pair();
        let opts = ResolverOptions {
            authority: "ignored".to_string(),
            runtime: rt::default_runtime(),
            work_scheduler: work_scheduler.clone(),
        };

        let builder = global_registry().get("unix").expect("scheme not found");
        let mut resolver = builder.build(&target, opts);

        // Wait for work to be scheduled.
        work_rx.recv().await.unwrap();

        let (mut channel_controller, mut update_rx) = TestChannelController::new_pair();
        resolver.work(&mut channel_controller);

        let update = update_rx.recv().await.unwrap();
        let endpoints = update.endpoints.expect("Should have succeeded");
        assert_eq!(endpoints.len(), 1);

        let addr = &endpoints[0].addresses[0];
        assert_eq!(addr.network_type, UNIX_NETWORK_TYPE);
        assert_eq!(&*addr.address, want_addr);
    }

    #[tokio::test]
    async fn unix_resolver_failure() {
        reg();

        let target: Target = "unix://authority/path"
            .parse()
            .expect("Failed to parse target");
        let (work_scheduler, mut work_rx) = TestWorkScheduler::new_pair();
        let opts = ResolverOptions {
            authority: "ignored".to_string(),
            runtime: rt::default_runtime(),
            work_scheduler: work_scheduler.clone(),
        };

        let builder = global_registry().get("unix").expect("scheme not found");
        let mut resolver = builder.build(&target, opts);

        // Wait for work to be scheduled.
        work_rx.recv().await.unwrap();

        let (mut channel_controller, mut update_rx) = TestChannelController::new_pair();
        resolver.work(&mut channel_controller);

        let update = update_rx.recv().await.unwrap();
        let err = update.endpoints.expect_err("Should have failed");
        assert!(err.contains("invalid (non-empty) authority"));
    }
}
