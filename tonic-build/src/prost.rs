use super::{client, server};
use proc_macro2::TokenStream;
use prost_build::{Config, Method, Service};
use quote::ToTokens;
use std::io;
use std::path::{Path, PathBuf};

/// Configure `tonic-build` code generation.
///
/// Use [`compile_protos`] instead if you don't need to tweak anything.
pub fn configure() -> Builder {
    Builder {
        build_client: true,
        build_server: true,
        file_descriptor_set_path: None,
        out_dir: None,
        extern_path: Vec::new(),
        field_attributes: Vec::new(),
        type_attributes: Vec::new(),
        proto_path: "super".to_string(),
        compile_well_known_types: false,
        #[cfg(feature = "rustfmt")]
        format: true,
        emit_package: true,
    }
}

/// Simple `.proto` compiling. Use [`configure`] instead if you need more options.
///
/// The include directory will be the parent folder of the specified path.
/// The package name will be the filename without the extension.
pub fn compile_protos(proto: impl AsRef<Path>) -> io::Result<()> {
    let proto_path: &Path = proto.as_ref();

    // directory the main .proto file resides in
    let proto_dir = proto_path
        .parent()
        .expect("proto file should reside in a directory");

    self::configure().compile(&[proto_path], &[proto_dir])?;

    Ok(())
}

const PROST_CODEC_PATH: &str = "tonic::codec::ProstCodec";

impl crate::Service for Service {
    const CODEC_PATH: &'static str = PROST_CODEC_PATH;

    type Method = Method;
    type Comment = String;

    fn name(&self) -> &str {
        &self.name
    }

    fn package(&self) -> &str {
        &self.package
    }

    fn identifier(&self) -> &str {
        &self.proto_name
    }

    fn comment(&self) -> &[Self::Comment] {
        &self.comments.leading[..]
    }

    fn methods(&self) -> &[Self::Method] {
        &self.methods[..]
    }
}

impl crate::Method for Method {
    const CODEC_PATH: &'static str = PROST_CODEC_PATH;
    type Comment = String;

    fn name(&self) -> &str {
        &self.name
    }

    fn identifier(&self) -> &str {
        &self.proto_name
    }

    fn client_streaming(&self) -> bool {
        self.client_streaming
    }

    fn server_streaming(&self) -> bool {
        self.server_streaming
    }

    fn comment(&self) -> &[Self::Comment] {
        &self.comments.leading[..]
    }

    fn request_response_name(
        &self,
        proto_path: &str,
        compile_well_known_types: bool,
    ) -> (TokenStream, TokenStream) {
        let request = if (self.input_proto_type.starts_with(".google.protobuf")
            && !compile_well_known_types)
            || self.input_type.starts_with("::")
        {
            self.input_type.parse::<TokenStream>().unwrap()
        } else {
            syn::parse_str::<syn::Path>(&format!("{}::{}", proto_path, self.input_type))
                .unwrap()
                .to_token_stream()
        };

        let response = if (self.output_proto_type.starts_with(".google.protobuf")
            && !compile_well_known_types)
            || self.output_type.starts_with("::")
        {
            self.output_type.parse::<TokenStream>().unwrap()
        } else {
            syn::parse_str::<syn::Path>(&format!("{}::{}", proto_path, self.output_type))
                .unwrap()
                .to_token_stream()
        };

        (request, response)
    }
}

struct ServiceGenerator {
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
        if self.builder.build_server {
            let server = server::generate(
                &service,
                self.builder.emit_package,
                &self.builder.proto_path,
                self.builder.compile_well_known_types,
            );
            self.servers.extend(server);
        }

        if self.builder.build_client {
            let client = client::generate(
                &service,
                self.builder.emit_package,
                &self.builder.proto_path,
                self.builder.compile_well_known_types,
            );
            self.clients.extend(client);
        }
    }

    fn finalize(&mut self, buf: &mut String) {
        if self.builder.build_client && !self.clients.is_empty() {
            let clients = &self.clients;

            let client_service = quote::quote! {
                #clients
            };

            let code = format!("{}", client_service);
            buf.push_str(&code);

            self.clients = TokenStream::default();
        }

        if self.builder.build_server && !self.servers.is_empty() {
            let servers = &self.servers;

            let server_service = quote::quote! {
                #servers
            };

            let code = format!("{}", server_service);
            buf.push_str(&code);

            self.servers = TokenStream::default();
        }
    }
}

/// Service generator builder.
#[derive(Debug, Clone)]
pub struct Builder {
    pub(crate) build_client: bool,
    pub(crate) build_server: bool,
    pub(crate) file_descriptor_set_path: Option<PathBuf>,
    pub(crate) extern_path: Vec<(String, String)>,
    pub(crate) field_attributes: Vec<(String, String)>,
    pub(crate) type_attributes: Vec<(String, String)>,
    pub(crate) proto_path: String,
    pub(crate) emit_package: bool,
    pub(crate) compile_well_known_types: bool,

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

    /// Generate a file containing the encoded `prost_types::FileDescriptorSet` for protocol buffers
    /// modules. This is required for implementing gRPC Server Reflection.
    pub fn file_descriptor_set_path(mut self, path: impl AsRef<Path>) -> Self {
        self.file_descriptor_set_path = Some(path.as_ref().to_path_buf());
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

    /// Set the path to where tonic will search for the Request/Response proto structs
    /// live relative to the module where you call `include_proto!`.
    ///
    /// This defaults to `super` since tonic will generate code in a module.
    pub fn proto_path(mut self, proto_path: impl AsRef<str>) -> Self {
        self.proto_path = proto_path.as_ref().to_string();
        self
    }

    /// Emits GRPC endpoints with no attached package. Effectively ignores protofile package declaration from grpc context.
    ///
    /// This effectively sets prost's exported package to an empty string.
    pub fn disable_package_emission(mut self) -> Self {
        self.emit_package = false;
        self
    }

    /// Enable or disable directing Prost to compile well-known protobuf types instead
    /// of using the already-compiled versions available in the `prost-types` crate.
    ///
    /// This defaults to `false`.
    pub fn compile_well_known_types(mut self, compile_well_known_types: bool) -> Self {
        self.compile_well_known_types = compile_well_known_types;
        self
    }

    /// Compile the .proto files and execute code generation.
    pub fn compile<P>(self, protos: &[P], includes: &[P]) -> io::Result<()>
    where
        P: AsRef<Path>,
    {
        self.compile_with_config(Config::new(), protos, includes)
    }

    /// Compile the .proto files and execute code generation using a
    /// custom `prost_build::Config`.
    pub fn compile_with_config<P>(
        self,
        mut config: Config,
        protos: &[P],
        includes: &[P],
    ) -> io::Result<()>
    where
        P: AsRef<Path>,
    {
        let out_dir = if let Some(out_dir) = self.out_dir.as_ref() {
            out_dir.clone()
        } else {
            PathBuf::from(std::env::var("OUT_DIR").unwrap())
        };

        #[cfg(feature = "rustfmt")]
        let format = self.format;

        config.out_dir(out_dir.clone());
        if let Some(path) = self.file_descriptor_set_path.as_ref() {
            config.file_descriptor_set_path(path);
        }
        for (proto_path, rust_path) in self.extern_path.iter() {
            config.extern_path(proto_path, rust_path);
        }
        for (prost_path, attr) in self.field_attributes.iter() {
            config.field_attribute(prost_path, attr);
        }
        for (prost_path, attr) in self.type_attributes.iter() {
            config.type_attribute(prost_path, attr);
        }
        if self.compile_well_known_types {
            config.compile_well_known_types();
        }
        config.service_generator(Box::new(ServiceGenerator::new(self)));

        config.compile_protos(protos, includes)?;

        #[cfg(feature = "rustfmt")]
        {
            if format {
                super::fmt(out_dir.to_str().expect("Expected utf8 out_dir"));
            }
        }

        Ok(())
    }
}
