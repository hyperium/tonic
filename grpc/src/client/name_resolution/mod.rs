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

//! Name Resolution for gRPC.
//!
//! Name Resolution is the process by which a channel's target is converted into
//! network addresses (typically IP addresses) used by the channel to connect to
//! a service.
use core::fmt;

use super::service_config::ServiceConfig;
use crate::{attributes::Attributes, byte_str::ByteStr, rt::Runtime};
use std::{
    fmt::{Display, Formatter},
    hash::{Hash, Hasher},
    str::FromStr,
    sync::Arc,
};

mod backoff;
mod dns;
mod registry;
pub use registry::global_registry;
use url::Url;

/// Target represents a target for gRPC, as specified in:
/// https://github.com/grpc/grpc/blob/master/doc/naming.md.
/// It is parsed from the target string that gets passed during channel creation
/// by the user. gRPC passes it to the resolver and the balancer.
///
/// If the target follows the naming spec, and the parsed scheme is registered
/// with gRPC, we will parse the target string according to the spec. If the
/// target does not contain a scheme or if the parsed scheme is not registered
/// (i.e. no corresponding resolver available to resolve the endpoint), we will
/// apply the default scheme, and will attempt to reparse it.
#[derive(Debug, Clone)]
pub struct Target {
    url: Url,
}

impl FromStr for Target {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<Url>() {
            Ok(url) => Ok(Target { url }),
            Err(err) => Err(err.to_string()),
        }
    }
}

impl From<url::Url> for Target {
    fn from(url: url::Url) -> Self {
        Target { url }
    }
}

/// Target represents a target for gRPC, as specified in:
/// https://github.com/grpc/grpc/blob/master/doc/naming.md.
/// It is parsed from the target string that gets passed during channel creation
/// by the user. gRPC passes it to the resolver and the balancer.
///
/// If the target follows the naming spec, and the parsed scheme is registered
/// with gRPC, we will parse the target string according to the spec. If the
/// target does not contain a scheme or if the parsed scheme is not registered
/// (i.e. no corresponding resolver available to resolve the endpoint), we will
/// apply the default scheme, and will attempt to reparse it.
impl Target {
    pub fn scheme(&self) -> &str {
        self.url.scheme()
    }

    /// The host part of the authority.
    pub fn authority_host(&self) -> &str {
        self.url.host_str().unwrap_or("")
    }

    /// The port part of the authority.
    pub fn authority_port(&self) -> Option<u16> {
        self.url.port()
    }

    /// Returns either host:port or host depending on the existence of the port
    /// in the authority.
    pub fn authority_host_port(&self) -> String {
        let host = self.authority_host();
        let port = self.authority_port();
        if let Some(port) = port {
            format!("{host}:{port}")
        } else {
            host.to_owned()
        }
    }

    /// Retrieves endpoint from `Url.path()`.
    pub fn path(&self) -> &str {
        self.url.path()
    }
}

impl Display for Target {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}://{}{}",
            self.scheme(),
            self.authority_host_port(),
            self.path()
        )
    }
}

/// A name resolver factory that produces Resolver instances used by the channel
/// to resolve network addresses for the target URI.
pub trait ResolverBuilder: Send + Sync {
    /// Builds a name resolver instance.
    ///
    /// Note that build must not fail.  Instead, an erroring Resolver may be
    /// returned that calls ChannelController.update() with an Err value.
    fn build(&self, target: &Target, options: ResolverOptions) -> Box<dyn Resolver>;

    /// Reports the URI scheme handled by this name resolver.
    fn scheme(&self) -> &str;

    /// Returns the default authority for a channel using this name resolver
    /// and target. This refers to the *dataplane authority* — the value used
    /// in the `:authority` header of HTTP/2 requests — and not to be confused
    /// with the authority portion of the target URI, which typically specifies
    /// the name of an external server used for name resolution.
    ///
    /// By default, this method returns the path portion of the target URI,
    /// with the leading prefix removed.
    fn default_authority(&self, target: &Target) -> String {
        let path = target.path();
        path.strip_prefix("/").unwrap_or(path).to_string()
    }

    /// Returns a bool indicating whether the input uri is valid to create a
    /// resolver.
    fn is_valid_uri(&self, uri: &Target) -> bool;
}

/// A collection of data configured on the channel that is constructing this
/// name resolver.
#[non_exhaustive]
pub struct ResolverOptions {
    /// The authority that will be used for the channel by default. This refers
    /// to the `:authority` value sent in HTTP/2 requests — the dataplane
    /// authority — and not the authority portion of the target URI, which is
    /// typically used to identify the name resolution server.
    ///
    /// This value is either the result of the `default_authority` method of
    /// this `ResolverBuilder`, or another string if the channel was explicitly
    /// configured to override the default.
    pub authority: String,

    /// The runtime which provides utilities to do async work.
    pub runtime: Arc<dyn Runtime>,

    /// A hook into the channel's work scheduler that allows the Resolver to
    /// request the ability to perform operations on the ChannelController.
    pub work_scheduler: Arc<dyn WorkScheduler>,
}

/// Used to asynchronously request a call into the Resolver's work method.
pub trait WorkScheduler: Send + Sync {
    // Schedules a call into the Resolver's work method.  If there is already a
    // pending work call that has not yet started, this may not schedule another
    // call.
    fn schedule_work(&self);
}

/// Resolver watches for the updates on the specified target.
/// Updates include address updates and service config updates.
// This trait may not need the Sync sub-trait if the channel implementation can
// ensure that the resolver is accessed serially. The sub-trait can be removed
// in that case.
pub trait Resolver: Send + Sync {
    /// Asks the resolver to obtain an updated resolver result, if applicable.
    ///
    /// This is useful for polling resolvers to decide when to re-resolve.
    /// However, the implementation is not required to re-resolve immediately
    /// upon receiving this call; it may instead elect to delay based on some
    /// configured minimum time between queries, to avoid hammering the name
    /// service with queries.
    ///
    /// For watch based resolvers, this may be a no-op.
    fn resolve_now(&mut self);

    /// Called serially by the channel to provide access to the
    /// `ChannelController`.
    fn work(&mut self, channel_controller: &mut dyn ChannelController);
}

/// The `ChannelController` trait provides the resolver with functionality
/// to interact with the channel.
pub trait ChannelController: Send + Sync {
    /// Notifies the channel about the current state of the name resolver.  If
    /// an error value is returned, the name resolver should attempt to
    /// re-resolve, if possible.  The resolver is responsible for applying an
    /// appropriate backoff mechanism to avoid overloading the system or the
    /// remote resolver.
    fn update(&mut self, update: ResolverUpdate) -> Result<(), String>;

    /// Parses the provided JSON service config and returns an instance of a
    /// ParsedServiceConfig.
    fn parse_service_config(&self, config: &str) -> Result<ServiceConfig, String>;
}

#[derive(Clone, Debug)]
#[non_exhaustive]
/// ResolverUpdate contains the current Resolver state relevant to the
/// channel.
pub struct ResolverUpdate {
    /// Attributes contains arbitrary data about the resolver intended for
    /// consumption by the load balancing policy.
    pub attributes: Attributes,

    /// A list of endpoints which each identify a logical host serving the
    /// service indicated by the target URI.
    pub endpoints: Result<Vec<Endpoint>, String>,

    /// The service config which the client should use for communicating with
    /// the service. If it is None, it indicates no service config is present or
    /// the resolver does not provide service configs.
    pub service_config: Result<Option<ServiceConfig>, String>,

    /// An optional human-readable note describing context about the
    /// resolution, to be passed along to the LB policy for inclusion in
    /// RPC failure status messages in cases where neither endpoints nor
    /// service_config has a non-OK status.  For example, a resolver that
    /// returns an empty endpoint list but a valid service config may set
    /// to this to something like "no DNS entries found for <name>".
    pub resolution_note: Option<String>,
}

impl Default for ResolverUpdate {
    fn default() -> Self {
        ResolverUpdate {
            service_config: Ok(Default::default()),
            attributes: Default::default(),
            endpoints: Ok(Default::default()),
            resolution_note: Default::default(),
        }
    }
}

/// An Endpoint is an address or a collection of addresses which reference one
/// logical server.  Multiple addresses may be used if there are multiple ways
/// which the server can be reached, e.g. via IPv4 and IPv6 addresses.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Endpoint {
    /// Addresses contains a list of addresses used to access this endpoint.
    pub addresses: Vec<Address>,

    /// Attributes contains arbitrary data about this endpoint intended for
    /// consumption by the LB policy.
    pub attributes: Attributes,
}

impl Hash for Endpoint {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.addresses.hash(state);
    }
}

/// An Address is an identifier that indicates how to connect to a server.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Ord, PartialOrd)]
pub struct Address {
    /// The network type is used to identify what kind of transport to create
    /// when connecting to this address.  Typically TCP_IP_ADDRESS_TYPE.
    pub network_type: &'static str,

    /// The address itself is passed to the transport in order to create a
    /// connection to it.
    pub address: ByteStr,

    /// Attributes contains arbitrary data about this address intended for
    /// consumption by the subchannel.
    pub attributes: Attributes,
}

impl Eq for Address {}

impl PartialEq for Address {
    fn eq(&self, other: &Self) -> bool {
        self.network_type == other.network_type && self.address == other.address
    }
}

impl Hash for Address {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.network_type.hash(state);
        self.address.hash(state);
    }
}

impl Display for Address {
    #[allow(clippy::to_string_in_format_args)]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.network_type, self.address.to_string())
    }
}

/// Indicates the address is an IPv4 or IPv6 address that should be connected to
/// via TCP/IP.
pub static TCP_IP_NETWORK_TYPE: &str = "tcp";

// A resolver that returns the same result every time its work method is called.
// It can be used to return an error to the channel when a resolver fails to
// build.
struct NopResolver {
    pub update: ResolverUpdate,
}

impl Resolver for NopResolver {
    fn resolve_now(&mut self) {}

    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        let _ = channel_controller.update(self.update.clone());
    }
}

#[cfg(test)]
mod test {
    use super::Target;

    #[test]
    pub fn parse_target() {
        #[derive(Default)]
        struct TestCase {
            input: &'static str,
            want_scheme: &'static str,
            want_host: &'static str,
            want_port: Option<u16>,
            want_host_port: &'static str,
            want_path: &'static str,
            want_str: &'static str,
        }
        let test_cases = vec![
            TestCase {
                input: "dns:///grpc.io",
                want_scheme: "dns",
                want_host_port: "",
                want_host: "",
                want_port: None,
                want_path: "/grpc.io",
                want_str: "dns:///grpc.io",
            },
            TestCase {
                input: "dns://8.8.8.8:53/grpc.io/docs",
                want_scheme: "dns",
                want_host_port: "8.8.8.8:53",
                want_host: "8.8.8.8",
                want_port: Some(53),
                want_path: "/grpc.io/docs",
                want_str: "dns://8.8.8.8:53/grpc.io/docs",
            },
            TestCase {
                input: "unix:path/to/file",
                want_scheme: "unix",
                want_host_port: "",
                want_host: "",
                want_port: None,
                want_path: "path/to/file",
                want_str: "unix://path/to/file",
            },
            TestCase {
                input: "unix:///run/containerd/containerd.sock",
                want_scheme: "unix",
                want_host_port: "",
                want_host: "",
                want_port: None,
                want_path: "/run/containerd/containerd.sock",
                want_str: "unix:///run/containerd/containerd.sock",
            },
        ];

        for tc in test_cases {
            let target: Target = tc.input.parse().unwrap();
            assert_eq!(target.scheme(), tc.want_scheme);
            assert_eq!(target.authority_host(), tc.want_host);
            assert_eq!(target.authority_port(), tc.want_port);
            assert_eq!(target.authority_host_port(), tc.want_host_port);
            assert_eq!(target.path(), tc.want_path);
            assert_eq!(&target.to_string(), tc.want_str);
        }
    }
}
