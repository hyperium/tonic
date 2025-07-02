//! Prost codec implementation for tonic.
//!
//! This crate provides the [`ProstCodec`] for encoding and decoding protobuf
//! messages using the [`prost`] library.
//!
//! # Example
//!
//! ```rust,ignore
//! use tonic_prost::ProstCodec;
//!
//! let codec = ProstCodec::<Message, Message>::default();
//! ```

#![warn(
    missing_docs,
    missing_debug_implementations,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/tokio-rs/website/master/public/img/icons/tonic.svg"
)]
#![doc(html_root_url = "https://docs.rs/tonic-prost/0.13.1")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

mod codec;

pub use codec::{ProstCodec, ProstDecoder, ProstEncoder};

// Re-export prost types that users might need
pub use prost;
