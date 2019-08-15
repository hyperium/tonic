use prost_build::Config;
use serde::Serialize;
use std::{io, path};

pub fn compile_protos<P>(protos: &[P], includes: &[P]) -> io::Result<()>
where
    P: AsRef<path::Path>,
{
    let mut config = Config::new();

    config.service_generator(Box::new(ServiceGenerator {}));

    config.compile_protos(protos, includes)
}

pub struct ServiceGenerator {}

impl prost_build::ServiceGenerator for ServiceGenerator {
    fn generate(&mut self, service: prost_build::Service, _buf: &mut String) {
        let file = format!(
            "{}/{}.{}.json",
            std::env::var("OUT_DIR").unwrap(),
            service.package,
            service.name
        );

        let svc = Service {
            name: service.name,
            proto_name: service.proto_name,
            package: service.package,
            methods: service
                .methods
                .into_iter()
                .map(|m| Method {
                    name: m.name,
                    proto_name: m.proto_name,
                    input_type: m.input_type,
                    output_type: m.output_type,
                    input_proto_type: m.input_proto_type,
                    output_proto_type: m.output_proto_type,
                    client_streaming: m.client_streaming,
                    server_streaming: m.server_streaming,
                })
                .collect(),
        };

        let json = serde_json::to_string(&svc).unwrap();

        std::fs::write(file, json).unwrap();
    }

    // fn finalize(&mut self, buf: &mut String) {
    //     let mut fmt = codegen::Formatter::new(buf);
    //     self.scope
    //         .fmt(&mut fmt)
    //         .expect("formatting root scope failed!");
    //     self.scope = codegen::Scope::new();
    // }
}

/// A service descriptor.
#[derive(Debug, Serialize)]
pub struct Service {
    /// The service name in Rust style.
    pub name: String,
    /// The service name as it appears in the .proto file.
    pub proto_name: String,
    /// The package name as it appears in the .proto file.
    pub package: String,
    /// The service methods.
    pub methods: Vec<Method>,
}

/// A service method descriptor.
#[derive(Debug, Serialize)]
pub struct Method {
    /// The name of the method in Rust style.
    pub name: String,
    /// The name of the method as it appears in the .proto file.
    pub proto_name: String,
    /// The input Rust type.
    pub input_type: String,
    /// The output Rust type.
    pub output_type: String,
    /// The input Protobuf type.
    pub input_proto_type: String,
    /// The output Protobuf type.
    pub output_proto_type: String,
    // /// The method options.
    // pub options: prost_types::MethodOptions,
    /// Identifies if client streams multiple client messages.
    pub client_streaming: bool,
    /// Identifies if server streams multiple server messages.
    pub server_streaming: bool,
}
