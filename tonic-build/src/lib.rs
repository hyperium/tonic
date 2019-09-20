//! `tonic-build` compiles `proto` files via `prost` and generates service stubs
//! and proto definitiones for use with `tonic`.
//!
//! # Examples
//!
//! ```rust,no_run
//! fn main() {
//!    tonic_build::compile_protos(
//!        &["proto/helloworld/helloworld.proto"],
//!        &["proto/helloworld"],
//!        "helloworld",
//!    )
//!    .unwrap();
//! }

use proc_macro2::TokenStream;
use prost_build::Config;
#[cfg(feature = "rustfmt")]
use std::process::Command;
use std::{io, path, path::Path};

mod client;
mod service;

use std::path::PathBuf;

#[derive(Clone)]
pub struct Builder {
    build_client: bool,
    build_server: bool,
    out_dir: PathBuf,
}

impl Builder {
    /// Enable or disable gRPC client code generation.
    pub fn build_client(&mut self, enable: bool) -> &mut Self {
        self.build_client = enable;
        self
    }

    /// Enable or disable gRPC server code generation.
    pub fn build_server(&mut self, enable: bool) -> &mut Self {
        self.build_server = enable;
        self
    }

    /// Set the output directory to generate code to.
    /// Defaults to the `OUT_DIR` environment variable.
    pub fn out_dir<P: AsRef<Path>>(&mut self, out_dir: impl AsRef<Path>) -> &mut Self {
        self.out_dir = out_dir.as_ref().to_path_buf();
        self
    }

    /// Compile the .proto files and execute code generation.
    #[allow(unused_variables)]
    pub fn compile<P: AsRef<Path>>(
        self,
        protos: &[P],
        includes: &[P],
        package: &str,
    ) -> io::Result<()> {
        let mut config = Config::new();

        config.service_generator(Box::new(ServiceGenerator::new(self.clone())));
        config.out_dir(self.out_dir);
        config.compile_protos(protos, includes)?;

        #[cfg(feature = "rustfmt")]
        fmt(
            out_dir.as_ref().to_str().expect("Execpted utf8 out_dir"),
            &format!("{}.rs", package),
        );

        Ok(())
    }
}

/// Configure tonic-build code generation. 
/// Use `compile_protos` instead if you don't need to tweak anything.
pub fn configure() -> Builder {
    Builder {
        build_client: true,
        build_server: true,
        out_dir: PathBuf::from(std::env::var("OUT_DIR").unwrap()),
    }
}

/// Easy .proto compiling. Use `configure` instead if you need more options.
pub fn compile_protos() -> io::Result<()> {
    unimplemented!()
    
    /*let proto_path = Path::new("proto");
    let protos = Vec::new();
    let includes = Vec::new();

    self::configure().compile()*/
}

#[cfg(feature = "rustfmt")]
fn fmt(out_dir: &str, file: &str) {
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
