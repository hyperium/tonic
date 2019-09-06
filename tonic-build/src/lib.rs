use proc_macro2::TokenStream;
use prost_build::Config;
use std::{io, path, path::Path, process::Command};

mod client;
mod service;

pub fn compile_protos<P>(protos: &[P], includes: &[P], package: &str) -> io::Result<()>
where
    P: AsRef<path::Path>,
{
    let out_dir = std::env::var("OUT_DIR").unwrap();
    compile_protos_with_out_dir(protos, includes, package, out_dir.as_str())
}

pub fn compile_protos_with_out_dir<P: AsRef<Path>>(
    protos: &[P],
    includes: &[P],
    package: &str,
    out_dir: impl AsRef<Path>,
) -> io::Result<()> {
    let mut config = Config::new();

    config.service_generator(Box::new(ServiceGenerator::default()));
    config.out_dir(out_dir.as_ref());
    config.compile_protos(protos, includes)?;

    fmt(
        out_dir.as_ref().to_str().expect("Execpted utf8 out_dir"),
        &format!("{}.rs", package),
    );

    Ok(())
}

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

#[derive(Default)]
pub struct ServiceGenerator {
    clients: TokenStream,
    servers: TokenStream,
}

impl prost_build::ServiceGenerator for ServiceGenerator {
    fn generate(&mut self, service: prost_build::Service, _buf: &mut String) {
        let path = "super";

        let server = service::generate(&service, path);
        self.servers.extend(server);

        let client = client::generate(&service, path);
        self.clients.extend(client);
    }

    fn finalize(&mut self, buf: &mut String) {
        if !self.clients.is_empty() && !self.servers.is_empty() {
            let clients = &self.clients;
            let servers = &self.servers;

            let service = quote::quote! {
                pub mod client {
                    #![allow(unused_variables, dead_code, missing_docs)]

                    #clients
                }

                pub mod server {
                    #![allow(unused_variables, dead_code, missing_docs)]

                    #servers
                }
            };

            let code = format!("{}", service);
            buf.push_str(&code);
        }
    }
}
