//! `tonic-build` compiles `proto` files via `prost` and generates service stubs
//! and proto definitiones for use with `tonic`.
//!
//! # Feature flags
//!
//! - `cleanup-markdown`: Enables cleaning up documentation from the generated code. Useful
//! when documentation of the generated code fails `cargo test --doc` for example.
//! - `prost`: Enables usage of prost generator (enabled by default).
//! - `transport`: Enables generation of `connect` method using `tonic::transport::Channel`
//! (enabled by default).
//!
//! # Required dependencies
//!
//! ```toml
//! [dependencies]
//! tonic = <tonic-version>
//! prost = <prost-version>
//!
//! [build-dependencies]
//! tonic-build = <tonic-version>
//! ```
//!
//! # Examples
//! Simple
//!
//! ```rust,no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     tonic_build::compile_protos("proto/service.proto")?;
//!     Ok(())
//! }
//! ```
//!
//! Configuration
//!
//! ```rust,no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!    tonic_build::configure()
//!         .build_server(false)
//!         .compile(
//!             &["proto/helloworld/helloworld.proto"],
//!             &["proto/helloworld"],
//!         )?;
//!    Ok(())
//! }
//!```
//!
//! ## NixOS related hints
//!
//! On NixOS, it is better to specify the location of `PROTOC` and `PROTOC_INCLUDE` explicitly.
//!
//! ```bash
//! $ export PROTOBUF_LOCATION=$(nix-env -q protobuf --out-path --no-name)
//! $ export PROTOC=$PROTOBUF_LOCATION/bin/protoc
//! $ export PROTOC_INCLUDE=$PROTOBUF_LOCATION/include
//! $ cargo build
//! ```
//!
//! The reason being that if `prost_build::compile_protos` fails to generate the resultant package,
//! the failure is not obvious until the `include!(concat!(env!("OUT_DIR"), "/resultant.rs"));`
//! fails with `No such file or directory` error.

#![recursion_limit = "256"]
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/tokio-rs/website/master/public/img/icons/tonic.svg"
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![doc(html_root_url = "https://docs.rs/tonic-build/0.8.2")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]
#![cfg_attr(docsrs, feature(doc_cfg))]

use proc_macro2::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream};
use quote::TokenStreamExt;

/// Prost generator
#[cfg(feature = "prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "prost")))]
mod prost;

#[cfg(feature = "prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "prost")))]
pub use prost::{compile_protos, configure, Builder};

pub mod manual;

/// Service code generation for client
pub mod client;
/// Service code generation for Server
pub mod server;

/// Service generation trait.
///
/// This trait can be implemented and consumed
/// by `client::generate` and `server::generate`
/// to allow any codegen module to generate service
/// abstractions.
pub trait Service {
    /// Comment type.
    type Comment: AsRef<str>;

    /// Method type.
    type Method: Method;

    /// Name of service.
    fn name(&self) -> &str;
    /// Package name of service.
    fn package(&self) -> &str;
    /// Identifier used to generate type name.
    fn identifier(&self) -> &str;
    /// Methods provided by service.
    fn methods(&self) -> &[Self::Method];
    /// Get comments about this item.
    fn comment(&self) -> &[Self::Comment];
}

/// Method generation trait.
///
/// Each service contains a set of generic
/// `Methods`'s that will be used by codegen
/// to generate abstraction implementations for
/// the provided methods.
pub trait Method {
    /// Comment type.
    type Comment: AsRef<str>;

    /// Name of method.
    fn name(&self) -> &str;
    /// Identifier used to generate type name.
    fn identifier(&self) -> &str;
    /// Path to the codec.
    fn codec_path(&self) -> &str;
    /// Method is streamed by client.
    fn client_streaming(&self) -> bool;
    /// Method is streamed by server.
    fn server_streaming(&self) -> bool;
    /// Get comments about this item.
    fn comment(&self) -> &[Self::Comment];
    /// Type name of request and response.
    fn request_response_name(
        &self,
        proto_path: &str,
        compile_well_known_types: bool,
    ) -> (TokenStream, TokenStream);
}

/// Attributes that will be added to `mod` and `struct` items.
#[derive(Debug, Default, Clone)]
pub struct Attributes {
    /// `mod` attributes.
    module: Vec<(String, String)>,
    /// `struct` attributes.
    structure: Vec<(String, String)>,
}

impl Attributes {
    fn for_mod(&self, name: &str) -> Vec<syn::Attribute> {
        generate_attributes(name, &self.module)
    }

    fn for_struct(&self, name: &str) -> Vec<syn::Attribute> {
        generate_attributes(name, &self.structure)
    }

    /// Add an attribute that will be added to `mod` items matching the given pattern.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tonic_build::*;
    /// let mut attributes = Attributes::default();
    /// attributes.push_mod("my.proto.package", r#"#[cfg(feature = "server")]"#);
    /// ```
    pub fn push_mod(&mut self, pattern: impl Into<String>, attr: impl Into<String>) {
        self.module.push((pattern.into(), attr.into()));
    }

    /// Add an attribute that will be added to `struct` items matching the given pattern.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tonic_build::*;
    /// let mut attributes = Attributes::default();
    /// attributes.push_struct("EchoService", "#[derive(PartialEq)]");
    /// ```
    pub fn push_struct(&mut self, pattern: impl Into<String>, attr: impl Into<String>) {
        self.structure.push((pattern.into(), attr.into()));
    }
}

// Generates attributes given a list of (`pattern`, `attribute`) pairs. If `pattern` matches `name`, `attribute` will be included.
fn generate_attributes<'a>(
    name: &str,
    attrs: impl IntoIterator<Item = &'a (String, String)>,
) -> Vec<syn::Attribute> {
    attrs
        .into_iter()
        .filter(|(matcher, _)| match_name(matcher, name))
        .flat_map(|(_, attr)| {
            // attributes cannot be parsed directly, so we pretend they're on a struct
            syn::parse_str::<syn::DeriveInput>(&format!("{}\nstruct fake;", attr))
                .unwrap()
                .attrs
        })
        .collect::<Vec<_>>()
}

// Generate a singular line of a doc comment
fn generate_doc_comment<S: AsRef<str>>(comment: S) -> TokenStream {
    let mut doc_stream = TokenStream::new();

    doc_stream.append(Ident::new("doc", Span::call_site()));
    doc_stream.append(Punct::new('=', Spacing::Alone));
    doc_stream.append(Literal::string(comment.as_ref()));

    let group = Group::new(Delimiter::Bracket, doc_stream);

    let mut stream = TokenStream::new();
    stream.append(Punct::new('#', Spacing::Alone));
    stream.append(group);
    stream
}

// Generate a larger doc comment composed of many lines of doc comments
fn generate_doc_comments<T: AsRef<str>>(comments: &[T]) -> TokenStream {
    let mut stream = TokenStream::new();

    for comment in comments {
        stream.extend(generate_doc_comment(comment));
    }

    stream
}

// Checks whether a path pattern matches a given path.
pub(crate) fn match_name(pattern: &str, path: &str) -> bool {
    if pattern.is_empty() {
        false
    } else if pattern == "." || pattern == path {
        true
    } else {
        let pattern_segments = pattern.split('.').collect::<Vec<_>>();
        let path_segments = path.split('.').collect::<Vec<_>>();

        if &pattern[..1] == "." {
            // prefix match
            if pattern_segments.len() > path_segments.len() {
                false
            } else {
                pattern_segments[..] == path_segments[..pattern_segments.len()]
            }
        // suffix match
        } else if pattern_segments.len() > path_segments.len() {
            false
        } else {
            pattern_segments[..] == path_segments[path_segments.len() - pattern_segments.len()..]
        }
    }
}

fn naive_snake_case(name: &str) -> String {
    let mut s = String::new();
    let mut it = name.chars().peekable();

    while let Some(x) = it.next() {
        s.push(x.to_ascii_lowercase());
        if let Some(y) = it.peek() {
            if y.is_uppercase() {
                s.push('_');
            }
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_name() {
        assert!(match_name(".", ".my.protos"));
        assert!(match_name(".", ".protos"));

        assert!(match_name(".my", ".my"));
        assert!(match_name(".my", ".my.protos"));
        assert!(match_name(".my.protos.Service", ".my.protos.Service"));

        assert!(match_name("Service", ".my.protos.Service"));

        assert!(!match_name(".m", ".my.protos"));
        assert!(!match_name(".p", ".protos"));

        assert!(!match_name(".my", ".myy"));
        assert!(!match_name(".protos", ".my.protos"));
        assert!(!match_name(".Service", ".my.protos.Service"));

        assert!(!match_name("service", ".my.protos.Service"));
    }

    #[test]
    fn test_snake_case() {
        for case in &[
            ("Service", "service"),
            ("ThatHasALongName", "that_has_a_long_name"),
            ("greeter", "greeter"),
            ("ABCServiceX", "a_b_c_service_x"),
        ] {
            assert_eq!(naive_snake_case(case.0), case.1)
        }
    }
}
