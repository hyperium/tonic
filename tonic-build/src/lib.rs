//! `tonic-build` compiles `proto` files via `prost` and generates service stubs
//! and proto definitiones for use with `tonic`.
//!
//! # Examples
//! Simple
//!
//! ```rust,no_run
//! fn main() {
//!     tonic_build::compile_protos().unwrap();
//! }
//! ```
//!
//! Configuration
//!
//! ```rust,no_run
//! fn main() {
//!    tonic_build::configure()
//!         .build_server(false)
//!         .compile(
//!             &["proto/helloworld/helloworld.proto"],
//!             &["proto/helloworld"],
//!             "helloworld",
//!         )
//!         .unwrap();
//! }
//! ```

use proc_macro2::TokenStream;
use prost_build::Config;
use walkdir::WalkDir;

use std::ffi::OsStr;
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
    out_dir: PathBuf,
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
    /// Defaults to the `OUT_DIR` environment variable.
    pub fn out_dir<P: AsRef<Path>>(mut self, out_dir: impl AsRef<Path>) -> Self {
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

        config.out_dir(self.out_dir.clone());
        config.service_generator(Box::new(ServiceGenerator::new(self)));
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
/// Use [`compile_protos`] instead if you don't need to tweak anything.
pub fn configure() -> Builder {
    Builder {
        build_client: true,
        build_server: true,
        out_dir: PathBuf::from(std::env::var("OUT_DIR").unwrap()),
    }
}

/// Simple .proto compiling. Use [`configure`] instead if you need more options.
///
/// This method will walk the `proto` folder in the same directory as Cargo.toml.
/// If a .proto file is found, it's parent folder will be the include directory for that proto file.
/// The package name for a given file will be the name of the file.
///
/// # Example
/// ```text
/// proto
/// ├── helloworld
/// │   └── helloworld.proto
/// └── routeguide
///     └── route_guide.proto
/// ```
///
/// For the above example, two .proto files will be generated.
/// One for the helloworld.proto file and another for routeguide.proto file.
///
pub fn compile_protos() -> io::Result<()> {
    for proto_entry in WalkDir::new("proto")
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension() == Some(OsStr::new("proto")))
    {
        let protos = &[proto_entry.path()];
        let package = proto_entry.path().file_stem().unwrap().to_str().unwrap();
        let includes = &[proto_entry.path().parent().unwrap()];

        self::configure().compile(protos, includes, package)?;
    }

    Ok(())
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
