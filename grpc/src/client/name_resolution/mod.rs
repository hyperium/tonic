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

use std::{
    error::Error,
    fmt::{Display, Formatter},
    hash::Hash,
    sync::Arc,
};

use async_trait::async_trait;
use tokio::sync::Notify;
use url::Url;

use crate::attributes::Attributes;

use super::service_config::ServiceConfig;

/// A name resolver factory that produces Resolver instances used by the channel
/// to resolve network addresses for the target URI.
pub trait ResolverBuilder: Send + Sync {
    /// Builds and returns a new name resolver instance.
    ///
    /// Note that build must not fail.  Instead, an erroring Resolver may be
    /// returned that calls ChannelController.update() with an Err value.
    fn build(
        &self,
        target: Url,
        resolve_now: Arc<Notify>,
        options: ResolverOptions,
    ) -> Box<dyn Resolver>;

    /// Reports the URI scheme handled by this name resolver.
    fn scheme(&self) -> &'static str;

    /// Returns the default authority for a channel using this name resolver and
    /// target.  This is typically the same as the service's name.  By default,
    /// the default_authority method automatically returns the path portion of
    /// the target URI, with the leading prefix removed.
    fn default_authority(&self, target: &Url) -> String {
        let path = target.path();
        path.strip_prefix("/").unwrap_or(path).to_string()
    }
}

/// A collection of data configured on the channel that is constructing this
/// name resolver.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct ResolverOptions {
    /// The authority that will be used for the channel by default.  This
    /// contains either the result of the default_authority method of this
    /// ResolverBuilder, or another string if the channel was configured to
    /// override the default.
    authority: String,
}

#[async_trait]
/// A collection of operations a Resolver may perform on the channel which
/// constructed it.
pub trait ChannelController: Send + Sync {
    /// Parses the provided JSON service config.
    fn parse_config(&self, config: &str) -> Result<ServiceConfig, Box<dyn Error + Send + Sync>>; // TODO

    /// Notifies the channel about the current state of the name resolver.  If
    /// an error value is returned, the name resolver should attempt to
    /// re-resolve, if possible.  The resolver is responsible for applying an
    /// appropriate backoff mechanism to avoid overloading the system or the
    /// remote resolver.
    async fn update(&self, update: ResolverUpdate) -> Result<(), Box<dyn Error + Send + Sync>>;
}

/// A name resolver update expresses the current state of the resolver.
pub enum ResolverUpdate {
    /// Indicates the name resolver encountered an error.
    Err(Box<dyn Error + Send + Sync>),
    /// Indicates the name resolver produced a valid result.
    Data(ResolverData),
}

/// Data provided by the name resolver to the channel.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct ResolverData {
    /// A list of endpoints which each identify a logical host serving the
    /// service indicated by the target URI.
    pub endpoints: Vec<Endpoint>,
    /// The service config which the client should use for communicating with
    /// the service.
    pub service_config: Option<ServiceConfig>,
    // Optional data which may be used by the LB Policy or channel.
    pub attributes: Attributes,
}

/// An Endpoint is an address or a collection of addresses which reference one
/// logical server.  Multiple addresses may be used if there are multiple ways
/// which the server can be reached, e.g. via IPv4 and IPv6 addresses.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Endpoint {
    /// The list of addresses used to connect to the server.
    pub addresses: Vec<Address>,
    /// Optional data which may be used by the LB policy or channel.
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

/// An Address is an identifier that indicates how to connect to a server.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Address {
    /// The address type is used to identify what kind of transport to create
    /// when connecting to this address.  Typically TCP_IP_ADDRESS_TYPE.
    pub address_type: String, // TODO: &'static str?
    /// The address itself is passed to the transport in order to create a
    /// connection to it.
    pub address: String,
    // Optional data which the transport may use for the connection.
    pub attributes: Attributes,
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
        write!(f, "{}:{}", self.address_type, self.address)
    }
}

/// Indicates the address is an IPv4 or IPv6 address that should be connected to
/// via TCP/IP.
pub static TCP_IP_ADDRESS_TYPE: &str = "tcp";

/// A name resolver instance.
#[async_trait]
pub trait Resolver: Send + Sync {
    /// The entry point of the resolver.  Will only be called once by the
    /// channel.  Should not return unless the resolver never will need to
    /// update its state.  The future will be dropped when the channel shuts
    /// down or enters idle mode.
    async fn run(&mut self, channel_controller: Box<dyn ChannelController>);
}
