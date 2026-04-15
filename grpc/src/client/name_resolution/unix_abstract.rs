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
        "unix-abstract"
    }

    fn is_valid_uri(&self, uri: &super::Target) -> bool {
        parse_target(uri).is_ok()
    }

    fn default_authority(&self, _target: &Target) -> String {
        "localhost".to_owned()
    }
}

/// Parses a target URI into an abstract UNIX domain socket address.
///
/// Valid format: `unix-abstract:abstract_path`
/// - `abstract_path` indicates a socket name in the abstract namespace.
/// - The name has no connection with filesystem pathnames and bypasses standard
///   filesystem permissions; any process or user may access the socket.
/// - The underlying system requires a null byte (`\0`) as the first character.
///   This function automatically prepends the null byte; it should not be
///   included it in `abstract_path`.
/// - Note: Abstract sockets are a Linux-specific kernel feature.
fn parse_target(target: &Target) -> Result<Address, String> {
    let host_port = target.authority_host_port();
    if !host_port.is_empty() {
        return Err(format!("invalid (non-empty) authority: {host_port}"));
    }
    let addr_string = format!("\0{}", target.path());
    Ok(Address {
        network_type: UNIX_NETWORK_TYPE,
        address: ByteStr::from(addr_string),
        attributes: Attributes::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::name_resolution::ResolverOptions;
    use crate::client::name_resolution::test_utils::TestChannelController;
    use crate::client::name_resolution::test_utils::TestWorkScheduler;
    use crate::rt;

    #[tokio::test]
    async fn unix_abstract_resolver() {
        reg();

        let target: Target = "unix-abstract:abstract_name"
            .parse()
            .expect("Failed to parse target");
        let (work_scheduler, mut work_rx) = TestWorkScheduler::new_pair();
        let opts = ResolverOptions {
            authority: "ignored".to_string(),
            runtime: rt::default_runtime(),
            work_scheduler: work_scheduler.clone(),
        };

        let builder = global_registry()
            .get("unix-abstract")
            .expect("scheme not found");
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
        assert_eq!(&*addr.address, "\0abstract_name");
    }
}
