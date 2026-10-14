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

use std::sync::Arc;
use std::sync::LazyLock;

use hyper_util::client::proxy::matcher::Matcher;
use url::Url;

use crate::client::name_resolution::ChannelController;
use crate::client::name_resolution::NopResolver;
use crate::client::name_resolution::Resolver;
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::ResolverOptions;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::Target;
use crate::client::name_resolution::dns;
use crate::client::service_config::ParseResult;
use crate::client::transport::ProxyOptions;
use crate::credentials::common::Authority;

static MATCHER: LazyLock<Option<Matcher>> = LazyLock::new(build_matcher);

fn build_matcher() -> Option<Matcher> {
    // Avoid using a proxy in a Common Gateway Interface (CGI) environment.
    if std::env::var_os("REQUEST_METHOD").is_some() {
        return None;
    }

    let https_proxy = get_first_env(&["HTTPS_PROXY", "https_proxy"]);
    if https_proxy.is_empty() {
        return None;
    }

    let builder = Matcher::builder();
    // Only read NO_PROXY and HTTPS_PROXY. This avoids reading ALL_PROXY,
    // which is not read by gRPC Go and C++.
    Some(
        builder
            .no(get_first_env(&["NO_PROXY", "no_proxy"]))
            .https(https_proxy)
            .build(),
    )
}

/// A resolver builder that wraps another `ResolverBuilder` and applies proxy
/// configuration.
///
/// This builder checks if the target URI should be proxied based on environment
/// variables (like `HTTPS_PROXY`, `NO_PROXY`). If a proxy is needed, it creates
/// a resolver that resolves the proxy address and injects proxy options into
/// the resolved addresses.
pub(crate) struct Builder {
    child_builder: Arc<dyn ResolverBuilder>,
}

impl ResolverBuilder for Builder {
    fn build(&self, target: &Target, options: ResolverOptions) -> Box<dyn Resolver> {
        self.new_resolver(target, options, MATCHER.as_ref())
    }

    fn scheme(&self) -> &str {
        self.child_builder.scheme()
    }

    fn is_valid_uri(&self, uri: &Target) -> bool {
        self.child_builder.is_valid_uri(uri)
    }

    fn default_authority(&self, target: &Target) -> String {
        self.child_builder.default_authority(target)
    }
}

impl Builder {
    /// Creates a new `Builder` that wraps the given `child_builder`.
    pub(crate) fn new_arc(child_builder: Arc<dyn ResolverBuilder>) -> Arc<dyn ResolverBuilder> {
        // Skip proxy lookup for non-DNS targets.
        if child_builder.scheme() != "dns" {
            return child_builder;
        }

        Arc::new(Self { child_builder })
    }

    fn new_resolver(
        &self,
        target: &Target,
        options: ResolverOptions,
        matcher: Option<&Matcher>,
    ) -> Box<dyn Resolver> {
        // If HTTPS_PROXY is unset, avoid parsing the target as a DNS hostname.
        let Some(matcher) = matcher else {
            return self.child_builder.build(target, options);
        };

        let target_authority = self.child_builder.default_authority(target);
        // Use the URL crate to validate the authority and punycode encode it.
        let target_authority = authority_with_default_port(&target_authority, 443);
        let url_obj = match Url::parse(&format!("https://{target_authority}")) {
            Ok(url) => url,
            Err(err) => {
                return NopResolver::new_with_err(
                    format!("invalid target host in URL: {err}"),
                    options,
                );
            }
        };

        // The URL omits the default port for the scheme (443 for HTTPS), so we
        // must explicitly add it.
        let host = url_obj.host_str().unwrap_or("");
        let port = url_obj.port().unwrap_or(443);
        let explicit_authority = format!("{host}:{port}");

        let uri = match http::Uri::builder()
            .scheme("https")
            .authority(explicit_authority.as_str())
            .path_and_query("/")
            .build()
        {
            Ok(uri) => uri,
            Err(err) => {
                // This should not error since the url crate parsed the host.
                return NopResolver::new_with_err(
                    format!("failed to parse target authority: {}", err),
                    options,
                );
            }
        };

        let Some(intercept) = matcher.intercept(&uri) else {
            return self.child_builder.build(target, options);
        };

        let mut proxy_authorization_header = intercept.basic_auth().cloned();
        if let Some(ref mut header) = proxy_authorization_header {
            header.set_sensitive(true);
        }

        let proxy_options = ProxyOptions::new(explicit_authority, proxy_authorization_header);

        let Some(proxy_host) = intercept.uri().authority() else {
            return NopResolver::new_with_err(
                format!("proxy URI missing authority: {}", intercept.uri()),
                options,
            );
        };

        // `proxy_host` is be a valid URL authority. Because the `url` crate
        // parses the target using the WHATWG standard, it allows unescaped `[]`
        // characters in the path. Therefore, we don't need to explicitly
        // percent-encode the host string when adding it to the target path.
        let target_str = format!("dns:///{}", proxy_host);
        let proxy_target: Target = match target_str.parse() {
            Ok(t) => t,
            Err(e) => {
                return NopResolver::new_with_err(
                    format!("failed to parse proxy target {target_str}: {e}"),
                    options,
                );
            }
        };

        let child = dns::Builder {}.build(&proxy_target, options);

        Box::new(HttpsProxyResolver {
            child,
            proxy_options: Arc::new(proxy_options),
        })
    }
}

struct HttpsProxyResolver {
    child: Box<dyn Resolver>,
    proxy_options: Arc<ProxyOptions>,
}

impl Resolver for HttpsProxyResolver {
    fn resolve_now(&mut self) {
        self.child.resolve_now();
    }

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        let mut interceptor = InterceptingController {
            inner: channel_controller,
            proxy_options: &self.proxy_options,
        };
        self.child.work(&mut interceptor);
    }
}

struct InterceptingController<'a> {
    inner: &'a mut dyn ChannelController,
    proxy_options: &'a Arc<ProxyOptions>,
}

impl ChannelController for InterceptingController<'_> {
    fn update(&mut self, mut update: ResolverUpdate) -> Result<(), String> {
        if let Ok(endpoints) = &mut update.endpoints {
            for endpoint in endpoints {
                for address in &mut endpoint.addresses {
                    ProxyOptions::add_to_addr(address, self.proxy_options.clone());
                }
            }
        }
        self.inner.update(update)
    }

    fn parse_service_config(&self, config: &str) -> ParseResult {
        self.inner.parse_service_config(config)
    }
}

fn get_first_env(names: &[&str]) -> String {
    for name in names {
        if let Ok(val) = std::env::var(name) {
            return val;
        }
    }

    String::new()
}

fn authority_with_default_port(host_port: &str, default_port: u16) -> String {
    let mut authority = Authority::from_host_port_str(host_port);
    if authority.port().is_none() {
        authority.set_port(Some(default_port));
    }
    authority.host_port_string()
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::net::IpAddr;
    use std::pin::Pin;
    use std::sync::Arc;

    use http::HeaderValue;

    use super::*;
    use crate::attributes::Attributes;
    use crate::byte_str::ByteStr;
    use crate::client::name_resolution::test_utils::TestChannelController;
    use crate::client::name_resolution::test_utils::TestWorkScheduler;
    use crate::core::Address;
    use crate::rt;
    use crate::rt::GrpcEndpoint;
    use crate::rt::GrpcRuntime;
    use crate::rt::Runtime;
    use crate::rt::Sleep;
    use crate::rt::TaskHandle;
    use crate::rt::TcpOptions;
    use crate::rt::tokio::TokioRuntime;

    const DIRECT_ADDRESS: &str = "1.2.3.4:5678";

    #[derive(Clone, Debug)]
    struct FakeDns {
        lookup_result: Result<Vec<IpAddr>, String>,
    }

    #[crate::async_trait]
    impl rt::DnsResolver for FakeDns {
        async fn lookup_host_name(&self, _: &str) -> Result<Vec<IpAddr>, String> {
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

    impl Runtime for FakeRuntime {
        fn spawn(
            &self,
            task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
        ) -> Box<dyn TaskHandle> {
            self.inner.spawn(task)
        }

        fn get_dns_resolver(
            &self,
            _: rt::ResolverOptions,
        ) -> Result<Box<dyn rt::DnsResolver>, String> {
            Ok(Box::new(self.dns.clone()))
        }

        fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn Sleep>> {
            self.inner.sleep(duration)
        }

        fn tcp_stream(
            &self,
            target: std::net::SocketAddr,
            opts: TcpOptions,
        ) -> Pin<Box<dyn Future<Output = Result<Box<dyn GrpcEndpoint>, String>> + Send>> {
            self.inner.tcp_stream(target, opts)
        }

        fn tcp_listener(
            &self,
            addr: std::net::SocketAddr,
        ) -> rt::BoxFuture<Result<Box<dyn rt::EndpointListener>, String>> {
            self.inner.tcp_listener(addr)
        }

        fn unix_listener(
            &self,
            path: std::path::PathBuf,
            opts: rt::UnixSocketOptions,
        ) -> rt::BoxFuture<Result<Box<dyn rt::EndpointListener>, String>> {
            self.inner.unix_listener(path, opts)
        }
    }

    struct MockResolverBuilder {}

    impl ResolverBuilder for MockResolverBuilder {
        fn build(&self, _target: &Target, options: ResolverOptions) -> Box<dyn Resolver> {
            let addr = Address {
                network_type: "tcp",
                address: ByteStr::from(DIRECT_ADDRESS.to_string()),
                attributes: Attributes::new(),
            };
            NopResolver::new_with_addr(addr, options)
        }

        fn scheme(&self) -> &str {
            "dns"
        }

        fn is_valid_uri(&self, _uri: &Target) -> bool {
            true
        }
    }

    async fn run_resolver_and_get_addresses_with_builder(
        target_uri: &str,
        dns_ips: Vec<IpAddr>,
        matcher: Option<&Matcher>,
        child_builder: Arc<dyn ResolverBuilder>,
    ) -> Vec<Address> {
        let builder = Builder { child_builder };

        let target: Target = target_uri.parse().unwrap();
        let (work_scheduler, mut work_rx) = TestWorkScheduler::new_pair();
        let runtime = FakeRuntime {
            inner: TokioRuntime::default(),
            dns: FakeDns {
                lookup_result: Ok(dns_ips),
            },
        };
        let options = ResolverOptions {
            authority: target.authority_host_port(),
            runtime: GrpcRuntime::new(runtime),
            work_scheduler,
        };

        let mut resolver = builder.new_resolver(&target, options, matcher);

        work_rx.recv().await.unwrap();

        let (mut channel_controller, mut update_rx) = TestChannelController::new_pair();
        resolver.work(&mut channel_controller);

        let update = update_rx.recv().await.unwrap();
        let endpoints = update.endpoints.unwrap();

        let mut addresses = Vec::new();
        for endpoint in endpoints {
            for address in endpoint.addresses {
                addresses.push(address);
            }
        }
        addresses
    }

    async fn run_resolver_and_get_addresses(
        target_uri: &str,
        dns_ips: Vec<IpAddr>,
        matcher: Option<&Matcher>,
    ) -> Vec<Address> {
        let child_builder = Arc::new(MockResolverBuilder {});
        run_resolver_and_get_addresses_with_builder(target_uri, dns_ips, matcher, child_builder)
            .await
    }

    #[tokio::test]
    async fn proxy_matched() {
        let matcher = Matcher::builder()
            .https("http://user:password@proxy.example.com:8080")
            .build();

        let dns_ips = vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()];
        let addresses =
            run_resolver_and_get_addresses("dns:///target.example.com", dns_ips, Some(&matcher))
                .await;

        assert_eq!(addresses.len(), 2);
        assert_eq!(&*addresses[0].address, "127.0.0.1:8080");
        assert_eq!(&*addresses[1].address, "[::1]:8080");

        let mut expected_header = HeaderValue::from_static("Basic dXNlcjpwYXNzd29yZA==");
        expected_header.set_sensitive(true);
        let expected_proxy_opts =
            ProxyOptions::new("target.example.com:443".to_string(), Some(expected_header));

        for address in &addresses {
            let proxy_opts = ProxyOptions::from_addr(address).expect("ProxyOptions not found");
            assert_eq!(proxy_opts, &expected_proxy_opts);
        }
    }

    #[tokio::test]
    async fn proxy_not_matched() {
        let matcher = Matcher::builder()
            .https("http://proxy.example.com:8080")
            .no("target.example.com")
            .build();

        let addresses = run_resolver_and_get_addresses(
            "dns:///target.example.com",
            vec!["127.0.0.1".parse().unwrap()],
            Some(&matcher),
        )
        .await;

        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, DIRECT_ADDRESS);
        assert!(ProxyOptions::from_addr(&addresses[0]).is_none());
    }

    #[tokio::test]
    async fn punycode_encoding() {
        let matcher = Matcher::builder()
            .https("http://proxy.example.com:8080")
            .build();

        let addresses = run_resolver_and_get_addresses(
            "dns:///täst.example.com",
            vec!["127.0.0.1".parse().unwrap()],
            Some(&matcher),
        )
        .await;

        assert_eq!(addresses.len(), 1);
        let proxy_opts = ProxyOptions::from_addr(&addresses[0]).expect("ProxyOptions not found");
        assert_eq!(
            proxy_opts,
            &ProxyOptions::new("xn--tst-qla.example.com:443".to_string(), None)
        );
    }

    #[tokio::test]
    async fn invalid_path_with_proxy_errors() {
        let matcher = Matcher::builder()
            .https("http://proxy.example.com:8080")
            .build();

        // The path has a space in the first segment of the path, which makes it
        // an invalid hostname.
        let target_uri = "dns:///var%20/run/grpc.sock";

        let child_builder = Arc::new(MockResolverBuilder {});
        let builder = Builder { child_builder };

        let target: Target = target_uri.parse().unwrap();
        let (work_scheduler, mut work_rx) = TestWorkScheduler::new_pair();
        let runtime = FakeRuntime {
            inner: TokioRuntime::default(),
            dns: FakeDns {
                lookup_result: Ok(vec!["127.0.0.1".parse().unwrap()]),
            },
        };
        let options = ResolverOptions {
            authority: target.authority_host_port(),
            runtime: GrpcRuntime::new(runtime),
            work_scheduler,
        };

        let mut resolver = builder.new_resolver(&target, options, Some(&matcher));

        work_rx.recv().await.unwrap();

        let (mut channel_controller, mut update_rx) = TestChannelController::new_pair();
        resolver.work(&mut channel_controller);

        let update = update_rx.recv().await.unwrap();
        let err = update.endpoints.unwrap_err();
        assert!(err.contains("invalid target host in URL"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_path_bypass() {
        crate::client::name_resolution::unix::reg();
        let unix_builder = crate::client::name_resolution::global_registry()
            .get("unix")
            .expect("unix resolver not registered");
        let proxy_builder = Builder::new_arc(unix_builder.clone());
        assert!(Arc::ptr_eq(&unix_builder, &proxy_builder));
    }

    #[tokio::test]
    async fn matcher_behavior_configured_manually() {
        let dns_ips = || vec!["127.0.0.1".parse().unwrap()];

        // Case 1: http proxy is set, but destination is HTTPS.
        // It should NOT be matched.
        let matcher = Matcher::builder()
            .http("http://proxy.example.com:8080")
            .build();
        let addresses =
            run_resolver_and_get_addresses("dns:///target.example.com", dns_ips(), Some(&matcher))
                .await;
        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, DIRECT_ADDRESS);
        assert!(
            ProxyOptions::from_addr(&addresses[0]).is_none(),
            "HTTP proxy should not match HTTPS destinations"
        );

        // Case 2: https proxy is set, destination is HTTPS.
        // It should be matched.
        let matcher = Matcher::builder()
            .https("http://proxy.example.com:8080")
            .build();
        let addresses =
            run_resolver_and_get_addresses("dns:///target.example.com", dns_ips(), Some(&matcher))
                .await;
        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, "127.0.0.1:8080");
        let expected_proxy_opts = ProxyOptions::new("target.example.com:443".to_string(), None);
        let proxy_opts = ProxyOptions::from_addr(&addresses[0]).expect("ProxyOptions not found");
        assert_eq!(proxy_opts, &expected_proxy_opts);

        // Case 3: https proxy and no proxy are configured.
        let matcher = Matcher::builder()
            .https("http://proxy.example.com:8080")
            .no("target.example.com")
            .build();

        // Target A: target.example.com (matched by no_proxy) -> should bypass proxy
        let addresses =
            run_resolver_and_get_addresses("dns:///target.example.com", dns_ips(), Some(&matcher))
                .await;
        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, DIRECT_ADDRESS);
        assert!(ProxyOptions::from_addr(&addresses[0]).is_none());

        // Target B: other.example.com (NOT matched by no_proxy) -> should proxy
        let addresses =
            run_resolver_and_get_addresses("dns:///other.example.com", dns_ips(), Some(&matcher))
                .await;
        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, "127.0.0.1:8080");
        let expected_proxy_opts = ProxyOptions::new("other.example.com:443".to_string(), None);
        let proxy_opts = ProxyOptions::from_addr(&addresses[0]).expect("ProxyOptions not found");
        assert_eq!(proxy_opts, &expected_proxy_opts);
    }

    #[tokio::test]
    async fn no_matcher_returns_child_resolver() {
        let addresses = run_resolver_and_get_addresses(
            "unix:///invalid/but/doesnt/matter/since/no/matcher",
            vec!["127.0.0.1".parse().unwrap()],
            None,
        )
        .await;

        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, DIRECT_ADDRESS);
        assert!(ProxyOptions::from_addr(&addresses[0]).is_none());
    }

    #[tokio::test]
    async fn ipv6_proxy_address() {
        let matcher = Matcher::builder().https("http://[::1]:8080").build();

        let addresses = run_resolver_and_get_addresses(
            "dns:///target.example.com",
            vec!["127.0.0.1".parse().unwrap()],
            Some(&matcher),
        )
        .await;

        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, "[::1]:8080");

        let expected_proxy_opts = ProxyOptions::new("target.example.com:443".to_string(), None);

        let proxy_opts = ProxyOptions::from_addr(&addresses[0]).expect("ProxyOptions not found");
        assert_eq!(proxy_opts, &expected_proxy_opts);
    }

    #[tokio::test]
    async fn ipv6_target_address() {
        let matcher = Matcher::builder()
            .https("http://proxy.example.com:8080")
            .build();

        let addresses = run_resolver_and_get_addresses(
            "dns:///::1",
            vec!["127.0.0.1".parse().unwrap()],
            Some(&matcher),
        )
        .await;

        assert_eq!(addresses.len(), 1);
        assert_eq!(&*addresses[0].address, "127.0.0.1:8080");

        let expected_proxy_opts = ProxyOptions::new("[::1]:443".to_string(), None);

        let proxy_opts = ProxyOptions::from_addr(&addresses[0]).expect("ProxyOptions not found");
        assert_eq!(proxy_opts, &expected_proxy_opts);
    }

    struct CustomAuthorityBuilder {
        default_authority: String,
    }

    impl ResolverBuilder for CustomAuthorityBuilder {
        fn build(&self, _target: &Target, options: ResolverOptions) -> Box<dyn Resolver> {
            let addr = Address {
                network_type: "tcp",
                address: ByteStr::from(DIRECT_ADDRESS.to_string()),
                attributes: Attributes::new(),
            };
            NopResolver::new_with_addr(addr, options)
        }

        fn scheme(&self) -> &str {
            "dns"
        }

        fn is_valid_uri(&self, _uri: &Target) -> bool {
            true
        }

        fn default_authority(&self, _target: &Target) -> String {
            self.default_authority.clone()
        }
    }

    #[tokio::test]
    async fn custom_resolver_builder_default_authority() {
        let matcher = Matcher::builder()
            .https("http://proxy.example.com:8080")
            .build();

        let custom_authority = "custom.authority.example.com:1234".to_string();
        let child_builder = Arc::new(CustomAuthorityBuilder {
            default_authority: custom_authority.clone(),
        });

        let dns_ips = vec!["127.0.0.1".parse().unwrap()];
        let addresses = run_resolver_and_get_addresses_with_builder(
            "dns:///whatever",
            dns_ips,
            Some(&matcher),
            child_builder,
        )
        .await;

        assert_eq!(addresses.len(), 1);
        let expected_proxy_opts =
            ProxyOptions::new("custom.authority.example.com:1234".to_string(), None);

        let proxy_opts = ProxyOptions::from_addr(&addresses[0]).expect("ProxyOptions not found");
        assert_eq!(proxy_opts, &expected_proxy_opts);
    }
}
