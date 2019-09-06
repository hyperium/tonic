use prost_build::Config;
use std::{io, path, process::Command};

mod client;
mod service;

pub fn compile_protos<P>(protos: &[P], includes: &[P], package: &str) -> io::Result<()>
where
    P: AsRef<path::Path>,
{
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let mut config = Config::new();

    config.service_generator(Box::new(ServiceGenerator {}));
    config.out_dir(&out_dir);
    config.compile_protos(protos, includes)?;

    fmt(&out_dir, &format!("{}.rs", package));

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

pub struct ServiceGenerator {}

impl prost_build::ServiceGenerator for ServiceGenerator {
    fn generate(&mut self, service: prost_build::Service, buf: &mut String) {
        let path = "self";
        let server = service::generate(&service, path);
        let code = format!("{}", server);
        buf.push_str(&code);

        let client = client::generate(&service, path);
        let code = format!("{}", client);
        buf.push_str(&code);
    }
}
