//! `tonic-build` compiles `proto` files via `prost` and generates service stubs
//! and proto definitiones for use with `tonic`.
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

use proc_macro2::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream};
use prost_build::Config;
use quote::TokenStreamExt;

#[cfg(feature = "rustfmt")]
use std::process::Command;
use std::{
    io,
    path::{Path, PathBuf},
};

mod client;
mod service;

#[derive(Clone)]
pub struct Builder {
    build_client: bool,
    build_server: bool,
    out_dir: Option<PathBuf>,
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

    /// Set the output directory to generate code to.
    ///
    /// Defaults to the `OUT_DIR` environment variable.
    pub fn out_dir(mut self, out_dir: impl AsRef<Path>) -> Self {
        self.out_dir = Some(out_dir.as_ref().to_path_buf());
        self
    }

    /// Compile the .proto files and execute code generation.
    pub fn compile<P: AsRef<Path>>(self, protos: &[P], includes: &[P]) -> io::Result<()> {
        let mut config = Config::new();

        let out_dir = self
            .out_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(std::env::var("OUT_DIR").unwrap()));

        config.out_dir(out_dir.clone());
        config.service_generator(Box::new(ServiceGenerator::new(self)));
        config.compile_protos(protos, includes)?;

        #[cfg(feature = "rustfmt")]
        fmt(out_dir.to_str().expect("Expected utf8 out_dir"));

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

#[cfg(feature = "rustfmt")]
fn fmt(out_dir: &str) {
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

pub struct ServiceGenerator {
    builder: Builder,
    clients: TokenStream,
    servers: TokenStream,
}

impl ServiceGenerator {
    fn new(builder: Builder) -> Self {
        ServiceGenerator {
            builder,
            clients: TokenStream::default(),
            servers: TokenStream::default(),
        }
    }
}

impl prost_build::ServiceGenerator for ServiceGenerator {
    fn generate(&mut self, service: prost_build::Service, _buf: &mut String) {
        let path = "super";

        if self.builder.build_server {
            let server = service::generate(&service, path);
            self.servers.extend(server);
        }

        if self.builder.build_client {
            let client = client::generate(&service, path);
            self.clients.extend(client);
        }
    }

    fn finalize(&mut self, buf: &mut String) {
        if self.builder.build_client && !self.clients.is_empty() {
            let clients = &self.clients;

            let client_service = quote::quote! {
                pub mod client {
                    #![allow(unused_variables, dead_code, missing_docs)]
                    use tonic::codegen::*;

                    #clients
                }
            };

            let code = format!("{}", client_service);
            buf.push_str(&code);
        }

        if self.builder.build_server && !self.servers.is_empty() {
            let servers = &self.servers;

            let server_service = quote::quote! {
                pub mod server {
                    #![allow(unused_variables, dead_code, missing_docs)]
                    use tonic::codegen::*;

                    #servers
                }
            };

            let code = format!("{}", server_service);
            buf.push_str(&code);
        }
    }
}

// Generate a singular line of a doc comment
fn generate_doc_comment(comment: &str, stream: &mut TokenStream) {
    let mut doc_stream = TokenStream::new();

    doc_stream.append(Ident::new("doc", Span::call_site()));
    doc_stream.append(Punct::new('=', Spacing::Alone));
    doc_stream.append(Literal::string(&comment));

    let group = Group::new(Delimiter::Bracket, doc_stream);

    stream.append(Punct::new('#', Spacing::Alone));
    stream.append(group);
}

// Generate a larger doc comment composed of many lines of doc comments
fn generate_doc_comments<T: AsRef<str>>(comments: &[T]) -> TokenStream {
    let mut stream = TokenStream::new();

    for comment in comments {
        generate_doc_comment(comment.as_ref(), &mut stream);
    }

    stream
}
