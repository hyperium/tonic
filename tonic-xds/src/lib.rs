pub mod client;
pub(crate) mod xds;

pub use client::channel::XdsChannel;

#[cfg(test)]
pub(crate) mod testutil;