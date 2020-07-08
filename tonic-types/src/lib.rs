//! A collection of useful protobuf types that can be used with `tonic`.

#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://github.com/hyperium/tonic/raw/master/.github/assets/tonic-docs.png"
)]
#![doc(html_root_url = "https://docs.rs/tonic-types/0.1.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

mod pb {
    include!(concat!(env!("OUT_DIR"), "/google.rpc.rs"));
}

pub use pb::Status;
