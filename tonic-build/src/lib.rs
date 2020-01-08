//! `tonic-build` compiles `proto` files via `prost` and generates service stubs
//! and proto definitiones for use with `tonic`.
//!
//! # Features
//!
//! - `rustfmt`: This feature enables the use of `rustfmt` to format the output code
//! this makes the code readable and the error messages nice. This requires that `rustfmt`
//! is installed. This is enabled by default.
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
//! ```

#![recursion_limit = "256"]
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://github.com/hyperium/tonic/raw/master/.github/assets/tonic-docs.png"
)]
#![doc(html_root_url = "https://docs.rs/tonic-build/0.1.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]

use proc_macro2::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream};
use quote::TokenStreamExt;

/// Prost generator
#[cfg(feature = "prost")]
pub mod prost;
/// Traits to describe schema
pub mod schema;

#[cfg(feature = "rustfmt")]
use std::process::Command;
use std::{
    io,
    path::{Path, PathBuf},
};

/// Serivce code generation for client
pub mod client;
/// Serivce code generation for Server
pub mod server;

/// Service generator builder.
#[derive(Debug, Clone)]
pub struct Builder {
    pub(crate) build_client: bool,
    pub(crate) build_server: bool,
    pub(crate) extern_path: Vec<(String, String)>,
    pub(crate) field_attributes: Vec<(String, String)>,
    pub(crate) type_attributes: Vec<(String, String)>,

    out_dir: Option<PathBuf>,
    #[cfg(feature = "rustfmt")]
    format: bool,
}

impl Builder {
    /// Enable or disable gRPC client code generation.
    pub fn build_client(mut self, enable: bool) -> Self {
        self.build_client = enable;
        self
    }

    /// Enable or disable gRPC server code generation.
    pub fn build_server(mut self, enable: bool) -> Self {
        self.build_server = enable;
        self
    }

    /// Enable the output to be formated by rustfmt.
    #[cfg(feature = "rustfmt")]
    pub fn format(mut self, run: bool) -> Self {
        self.format = run;
        self
    }

    /// Set the output directory to generate code to.
    ///
    /// Defaults to the `OUT_DIR` environment variable.
    pub fn out_dir(mut self, out_dir: impl AsRef<Path>) -> Self {
        self.out_dir = Some(out_dir.as_ref().to_path_buf());
        self
    }

    /// Declare externally provided Protobuf package or type.
    ///
    /// Passed directly to `prost_build::Config.extern_path`.
    /// Note that both the Protobuf path and the rust package paths should both be fully qualified.
    /// i.e. Protobuf paths should start with "." and rust paths should start with "::"
    pub fn extern_path(mut self, proto_path: impl AsRef<str>, rust_path: impl AsRef<str>) -> Self {
        self.extern_path.push((
            proto_path.as_ref().to_string(),
            rust_path.as_ref().to_string(),
        ));
        self
    }

    /// Add additional attribute to matched messages, enums, and one-offs.
    ///
    /// Passed directly to `prost_build::Config.field_attribute`.
    pub fn field_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.field_attributes
            .push((path.as_ref().to_string(), attribute.as_ref().to_string()));
        self
    }

    /// Add additional attribute to matched messages, enums, and one-offs.
    ///
    /// Passed directly to `prost_build::Config.type_attribute`.
    pub fn type_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.type_attributes
            .push((path.as_ref().to_string(), attribute.as_ref().to_string()));
        self
    }

    /// Compile the .proto files and execute code generation.
    pub fn compile<P: AsRef<Path>>(self, _protos: &[P], _includes: &[P]) -> io::Result<()> {
        let _out_dir = if let Some(out_dir) = self.out_dir.as_ref() {
            out_dir.clone()
        } else {
            PathBuf::from(std::env::var("OUT_DIR").unwrap())
        };

        #[cfg(feature = "rustfmt")]
        let format = self.format;

        #[cfg(feature = "prost")]
        prost::compile(self, _out_dir.clone(), _protos, _includes)?;

        #[cfg(feature = "rustfmt")]
        {
            if format {
                fmt(_out_dir.to_str().expect("Expected utf8 out_dir"));
            }
        }

        Ok(())
    }
}

/// Configure tonic-build code generation.
///
/// Use [`compile_protos`] instead if you don't need to tweak anything.
pub fn configure() -> Builder {
    Builder {
        build_client: true,
        build_server: true,
        out_dir: None,
        extern_path: Vec::new(),
        field_attributes: Vec::new(),
        type_attributes: Vec::new(),
        #[cfg(feature = "rustfmt")]
        format: true,
    }
}

/// Simple `.proto` compiling. Use [`configure`] instead if you need more options.
///
/// The include directory will be the parent folder of the specified path.
/// The package name will be the filename without the extension.
pub fn compile_protos(proto_path: impl AsRef<Path>) -> io::Result<()> {
    let proto_path: &Path = proto_path.as_ref();

    // directory the main .proto file resides in
    let proto_dir = proto_path
        .parent()
        .expect("proto file should reside in a directory");

    self::configure().compile(&[proto_path], &[proto_dir])?;

    Ok(())
}

/// Format files under the out_dir with rustfmt
#[cfg(feature = "rustfmt")]
pub fn fmt(out_dir: &str) {
    let dir = std::fs::read_dir(out_dir).unwrap();

    for entry in dir {
        let file = entry.unwrap().file_name().into_string().unwrap();
        let out = Command::new("rustfmt")
            .arg("--emit")
            .arg("files")
            .arg("--edition")
            .arg("2018")
            .arg(format!("{}/{}", out_dir, file))
            .output()
            .unwrap();

        println!("out: {:?}", out);
        assert!(out.status.success());
    }
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
fn generate_doc_comments<'a, T: AsRef<str> + 'a, C: IntoIterator<Item = &'a T>>(
    comments: C,
) -> TokenStream {
    let mut stream = TokenStream::new();

    for comment in comments {
        stream.extend(generate_doc_comment(comment));
    }

    stream
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
