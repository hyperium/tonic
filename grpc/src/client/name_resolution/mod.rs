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

//! Name Resolution for gRPC.
//!
//! Name Resolution is the process by which a channel's target is converted into
//! network addresses (typically IP addresses) used by the channel to connect to
//! a service.
use core::fmt;

use super::service_config::ServiceConfig;
use crate::{attributes::Attributes, rt};
use std::{
    fmt::{Display, Formatter},
    hash::Hash,
    str::FromStr,
    sync::Arc,
};

mod backoff;
mod dns;
mod registry;
pub use registry::global_registry;

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
    url: url::Url,
}

impl FromStr for Target {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<url::Url>() {
            Ok(url) => Ok(Target { url }),
            Err(err) => Err(err.to_string()),
        }
    }
}

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
            format!("{}:{}", host, port)
        } else {
            host.to_owned()
        }
    }

    /// Return the path for this target URL, as a percent-encoded ASCII string.
    pub fn path(&self) -> &str {
        self.url.path()
    }
}

impl Display for Target {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}//{}/{}",
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
    fn scheme(&self) -> &'static str;

    /// Returns the default authority for a channel using this name resolver
    /// and target.  This is typically the same as the service's name. By
    /// default, the default_authority method automatically returns the path
    /// portion of the target URI, with the leading prefix removed.
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
    /// The authority that will be used for the channel by default.  This
    /// contains either the result of the default_authority method of this
    /// ResolverBuilder, or another string if the channel was configured to
    /// override the default.
    pub authority: String,

    /// The runtime which provides utilities to do async work.
    pub runtime: Arc<dyn rt::Runtime>,

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
pub trait Resolver: Send {
    /// Asks the resolver to obtain an updated resolver result, if
    /// applicable.
    ///
    /// This is useful for pull-based implementations to decide when to
    /// re-resolve.  However, the implementation is not required to
    /// re-resolve immediately upon receiving this call; it may instead
    /// elect to delay based on some configured minimum time between
    /// queries, to avoid hammering the name service with queries.
    ///
    /// For push-based implementations, this may be a no-op.
    fn resolve_now(&mut self);

    /// Called serially by the channel to do work after the work scheduler's
    /// schedule_work method is called.
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
    pub attributes: Arc<Attributes>,

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
            service_config: Ok(None),
            attributes: Arc::default(),
            endpoints: Ok(Vec::default()),
            resolution_note: None,
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

/// An Address is an identifier that indicates how to connect to a server.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct Address {
    /// The network type is used to identify what kind of transport to create
    /// when connecting to this address.  Typically TCP_IP_ADDRESS_TYPE.
    pub network_type: &'static str,

    /// The address itself is passed to the transport in order to create a
    /// connection to it.
    pub address: String,

    /// Attributes contains arbitrary data about this address intended for
    /// consumption by the subchannel.
    pub attributes: Attributes,
}

impl Eq for Endpoint {}

impl PartialEq for Endpoint {
    fn eq(&self, _other: &Self) -> bool {
        todo!()
    }
}

impl Hash for Endpoint {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {
        todo!()
    }
}

impl Eq for Address {}

impl PartialEq for Address {
    fn eq(&self, _other: &Self) -> bool {
        todo!()
    }
}

impl Hash for Address {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {
        todo!()
    }
}

impl Display for Address {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.network_type, self.address)
    }
}

/// Indicates the address is an IPv4 or IPv6 address that should be connected to
/// via TCP/IP.
pub static TCP_IP_NETWORK_TYPE: &str = "tcp";

// A resolver that returns the same result every time it's work method is called.
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
        }
        let test_cases = vec![
            TestCase {
                input: "dns:///grpc.io",
                want_scheme: "dns",
                want_host_port: "",
                want_host: "",
                want_port: None,
                want_path: "/grpc.io",
            },
            TestCase {
                input: "dns://8.8.8.8:53/grpc.io/docs",
                want_scheme: "dns",
                want_host_port: "8.8.8.8:53",
                want_host: "8.8.8.8",
                want_port: Some(53),
                want_path: "/grpc.io/docs",
            },
            TestCase {
                input: "unix:path/to/file",
                want_scheme: "unix",
                want_host_port: "",
                want_host: "",
                want_port: None,
                want_path: "path/to/file",
            },
            TestCase {
                input: "unix:///run/containerd/containerd.sock",
                want_scheme: "unix",
                want_host_port: "",
                want_host: "",
                want_port: None,
                want_path: "/run/containerd/containerd.sock",
            },
        ];

        for tc in test_cases {
            let target: Target = tc.input.parse().unwrap();
            assert_eq!(target.scheme(), tc.want_scheme);
            assert_eq!(target.authority_host(), tc.want_host);
            assert_eq!(target.authority_port(), tc.want_port);
            assert_eq!(target.authority_host_port(), tc.want_host_port);
            assert_eq!(target.path(), tc.want_path);
        }
    }
}
