use crate::code_gen::CodeGenBuilder;

use super::Attributes;
use proc_macro2::TokenStream;
use prost_build::{Config, Method, Service};
use quote::ToTokens;
use std::{
    collections::HashSet,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

/// Configure `tonic-build` code generation.
///
/// Use [`compile_protos`] instead if you don't need to tweak anything.
pub fn configure() -> Builder {
    Builder {
        build_client: true,
        build_server: true,
        build_transport: true,
        file_descriptor_set_path: None,
        skip_protoc_run: false,
        out_dir: None,
        extern_path: Vec::new(),
        field_attributes: Vec::new(),
        message_attributes: Vec::new(),
        enum_attributes: Vec::new(),
        type_attributes: Vec::new(),
        boxed: Vec::new(),
        btree_map: None,
        bytes: None,
        server_attributes: Attributes::default(),
        client_attributes: Attributes::default(),
        proto_path: "super".to_string(),
        compile_well_known_types: false,
        emit_package: true,
        protoc_args: Vec::new(),
        include_file: None,
        emit_rerun_if_changed: std::env::var_os("CARGO").is_some(),
        disable_comments: HashSet::default(),
        use_arc_self: false,
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

/// Non-path Rust types allowed for request/response types.
const NON_PATH_TYPE_ALLOWLIST: &[&str] = &["()"];

impl crate::Service for Service {
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
    type Comment = String;

    fn name(&self) -> &str {
        &self.name
    }

    fn identifier(&self) -> &str {
        &self.proto_name
    }

    fn codec_path(&self) -> &str {
        PROST_CODEC_PATH
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
        let convert_type = |proto_type: &str, rust_type: &str| -> TokenStream {
            if (is_google_type(proto_type) && !compile_well_known_types)
                || rust_type.starts_with("::")
                || NON_PATH_TYPE_ALLOWLIST.iter().any(|ty| *ty == rust_type)
            {
                rust_type.parse::<TokenStream>().unwrap()
            } else if rust_type.starts_with("crate::") {
                syn::parse_str::<syn::Path>(rust_type)
                    .unwrap()
                    .to_token_stream()
            } else {
                syn::parse_str::<syn::Path>(&format!("{}::{}", proto_path, rust_type))
                    .unwrap()
                    .to_token_stream()
            }
        };

        let request = convert_type(&self.input_proto_type, &self.input_type);
        let response = convert_type(&self.output_proto_type, &self.output_type);
        (request, response)
    }
}

fn is_google_type(ty: &str) -> bool {
    ty.starts_with(".google.protobuf")
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
            let server = CodeGenBuilder::new()
                .emit_package(self.builder.emit_package)
                .compile_well_known_types(self.builder.compile_well_known_types)
                .attributes(self.builder.server_attributes.clone())
                .disable_comments(self.builder.disable_comments.clone())
                .use_arc_self(self.builder.use_arc_self)
                .generate_server(&service, &self.builder.proto_path);

            self.servers.extend(server);
        }

        if self.builder.build_client {
            let client = CodeGenBuilder::new()
                .emit_package(self.builder.emit_package)
                .compile_well_known_types(self.builder.compile_well_known_types)
                .attributes(self.builder.client_attributes.clone())
                .disable_comments(self.builder.disable_comments.clone())
                .build_transport(self.builder.build_transport)
                .generate_client(&service, &self.builder.proto_path);

            self.clients.extend(client);
        }
    }

    fn finalize(&mut self, buf: &mut String) {
        if self.builder.build_client && !self.clients.is_empty() {
            let clients = &self.clients;

            let client_service = quote::quote! {
                #clients
            };

            let ast: syn::File = syn::parse2(client_service).expect("not a valid tokenstream");
            let code = prettyplease::unparse(&ast);
            buf.push_str(&code);

            self.clients = TokenStream::default();
        }

        if self.builder.build_server && !self.servers.is_empty() {
            let servers = &self.servers;

            let server_service = quote::quote! {
                #servers
            };

            let ast: syn::File = syn::parse2(server_service).expect("not a valid tokenstream");
            let code = prettyplease::unparse(&ast);
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
    pub(crate) build_transport: bool,
    pub(crate) file_descriptor_set_path: Option<PathBuf>,
    pub(crate) skip_protoc_run: bool,
    pub(crate) extern_path: Vec<(String, String)>,
    pub(crate) field_attributes: Vec<(String, String)>,
    pub(crate) type_attributes: Vec<(String, String)>,
    pub(crate) message_attributes: Vec<(String, String)>,
    pub(crate) enum_attributes: Vec<(String, String)>,
    pub(crate) boxed: Vec<String>,
    pub(crate) btree_map: Option<Vec<String>>,
    pub(crate) bytes: Option<Vec<String>>,
    pub(crate) server_attributes: Attributes,
    pub(crate) client_attributes: Attributes,
    pub(crate) proto_path: String,
    pub(crate) emit_package: bool,
    pub(crate) compile_well_known_types: bool,
    pub(crate) protoc_args: Vec<OsString>,
    pub(crate) include_file: Option<PathBuf>,
    pub(crate) emit_rerun_if_changed: bool,
    pub(crate) disable_comments: HashSet<String>,
    pub(crate) use_arc_self: bool,

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

    /// Enable or disable generated clients and servers to have built-in tonic
    /// transport features.
    ///
    /// When the `transport` feature is disabled this does nothing.
    pub fn build_transport(mut self, enable: bool) -> Self {
        self.build_transport = enable;
        self
    }

    /// Generate a file containing the encoded `prost_types::FileDescriptorSet` for protocol buffers
    /// modules. This is required for implementing gRPC Server Reflection.
    pub fn file_descriptor_set_path(mut self, path: impl AsRef<Path>) -> Self {
        self.file_descriptor_set_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// In combination with with file_descriptor_set_path, this can be used to provide a file
    /// descriptor set as an input file, rather than having prost-build generate the file by
    /// calling protoc.
    pub fn skip_protoc_run(mut self) -> Self {
        self.skip_protoc_run = true;
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

    /// Add additional attribute to matched messages.
    ///
    /// Passed directly to `prost_build::Config.message_attribute`.
    pub fn message_attribute<P: AsRef<str>, A: AsRef<str>>(
        mut self,
        path: P,
        attribute: A,
    ) -> Self {
        self.message_attributes
            .push((path.as_ref().to_string(), attribute.as_ref().to_string()));
        self
    }

    /// Add additional attribute to matched enums.
    ///
    /// Passed directly to `prost_build::Config.enum_attribute`.
    pub fn enum_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.enum_attributes
            .push((path.as_ref().to_string(), attribute.as_ref().to_string()));
        self
    }

    /// Add additional boxed fields.
    ///
    /// Passed directly to `prost_build::Config.boxed`.
    pub fn boxed<P: AsRef<str>>(mut self, path: P) -> Self {
        self.boxed.push(path.as_ref().to_string());
        self
    }

    /// Configure the code generator to generate Rust `BTreeMap` fields for Protobuf `map` type
    /// fields.
    ///
    /// Passed directly to `prost_build::Config.btree_map`.
    ///
    /// Note: previous configurated paths for `btree_map` will be cleared.
    pub fn btree_map<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.btree_map = Some(
            paths
                .into_iter()
                .map(|path| path.as_ref().to_string())
                .collect(),
        );
        self
    }

    /// Configure the code generator to generate Rust `bytes::Bytes` fields for Protobuf `bytes`
    /// type fields.
    ///
    /// Passed directly to `prost_build::Config.bytes`.
    ///
    /// Note: previous configurated paths for `bytes` will be cleared.
    pub fn bytes<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.bytes = Some(
            paths
                .into_iter()
                .map(|path| path.as_ref().to_string())
                .collect(),
        );
        self
    }

    /// Add additional attribute to matched server `mod`s. Matches on the package name.
    pub fn server_mod_attribute<P: AsRef<str>, A: AsRef<str>>(
        mut self,
        path: P,
        attribute: A,
    ) -> Self {
        self.server_attributes
            .push_mod(path.as_ref().to_string(), attribute.as_ref().to_string());
        self
    }

    /// Add additional attribute to matched service servers. Matches on the service name.
    pub fn server_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.server_attributes
            .push_struct(path.as_ref().to_string(), attribute.as_ref().to_string());
        self
    }

    /// Add additional attribute to matched client `mod`s. Matches on the package name.
    pub fn client_mod_attribute<P: AsRef<str>, A: AsRef<str>>(
        mut self,
        path: P,
        attribute: A,
    ) -> Self {
        self.client_attributes
            .push_mod(path.as_ref().to_string(), attribute.as_ref().to_string());
        self
    }

    /// Add additional attribute to matched service clients. Matches on the service name.
    pub fn client_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.client_attributes
            .push_struct(path.as_ref().to_string(), attribute.as_ref().to_string());
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

    /// Configure Prost `protoc_args` build arguments.
    ///
    /// Note: Enabling `--experimental_allow_proto3_optional` requires protobuf >= 3.12.
    pub fn protoc_arg<A: AsRef<str>>(mut self, arg: A) -> Self {
        self.protoc_args.push(arg.as_ref().into());
        self
    }

    /// Disable service and rpc comments emission.
    pub fn disable_comments(mut self, path: impl AsRef<str>) -> Self {
        self.disable_comments.insert(path.as_ref().to_string());
        self
    }

    /// Emit `Arc<Self>` receiver type in server traits instead of `&self`.
    pub fn use_arc_self(mut self, enable: bool) -> Self {
        self.use_arc_self = enable;
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

    /// Configures the optional module filename for easy inclusion of all generated Rust files
    ///
    /// If set, generates a file (inside the `OUT_DIR` or `out_dir()` as appropriate) which contains
    /// a set of `pub mod XXX` statements combining to load all Rust files generated.  This can allow
    /// for a shortcut where multiple related proto files have been compiled together resulting in
    /// a semi-complex set of includes.
    pub fn include_file(mut self, path: impl AsRef<Path>) -> Self {
        self.include_file = Some(path.as_ref().to_path_buf());
        self
    }

    /// Enable or disable emitting
    /// [`cargo:rerun-if-changed=PATH`](https://doc.rust-lang.org/cargo/reference/build-scripts.html#rerun-if-changed)
    /// instructions for Cargo.
    ///
    /// If set, writes instructions to `stdout` for Cargo so that it understands
    /// when to rerun the build script. By default, this setting is enabled if
    /// the `CARGO` environment variable is set. The `CARGO` environment
    /// variable is set by Cargo for build scripts. Therefore, this setting
    /// should be enabled automatically when run from a build script. However,
    /// the method of detection is not completely reliable since the `CARGO`
    /// environment variable can have been set by anything else. If writing the
    /// instructions to `stdout` is undesireable, you can disable this setting
    /// explicitly.
    pub fn emit_rerun_if_changed(mut self, enable: bool) -> Self {
        self.emit_rerun_if_changed = enable;
        self
    }

    /// Compile the .proto files and execute code generation.
    pub fn compile(
        self,
        protos: &[impl AsRef<Path>],
        includes: &[impl AsRef<Path>],
    ) -> io::Result<()> {
        self.compile_with_config(Config::new(), protos, includes)
    }

    /// Compile the .proto files and execute code generation using a
    /// custom `prost_build::Config`.
    pub fn compile_with_config(
        self,
        mut config: Config,
        protos: &[impl AsRef<Path>],
        includes: &[impl AsRef<Path>],
    ) -> io::Result<()> {
        let out_dir = if let Some(out_dir) = self.out_dir.as_ref() {
            out_dir.clone()
        } else {
            PathBuf::from(std::env::var("OUT_DIR").unwrap())
        };

        config.out_dir(out_dir);
        if let Some(path) = self.file_descriptor_set_path.as_ref() {
            config.file_descriptor_set_path(path);
        }
        if self.skip_protoc_run {
            config.skip_protoc_run();
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
        for (prost_path, attr) in self.message_attributes.iter() {
            config.message_attribute(prost_path, attr);
        }
        for (prost_path, attr) in self.enum_attributes.iter() {
            config.enum_attribute(prost_path, attr);
        }
        for prost_path in self.boxed.iter() {
            config.boxed(prost_path);
        }
        if let Some(ref paths) = self.btree_map {
            config.btree_map(paths);
        }
        if let Some(ref paths) = self.bytes {
            config.bytes(paths);
        }
        if self.compile_well_known_types {
            config.compile_well_known_types();
        }
        if let Some(path) = self.include_file.as_ref() {
            config.include_file(path);
        }

        for arg in self.protoc_args.iter() {
            config.protoc_arg(arg);
        }

        if self.emit_rerun_if_changed {
            for path in protos.iter() {
                println!("cargo:rerun-if-changed={}", path.as_ref().display())
            }

            for path in includes.iter() {
                // Cargo will watch the **entire** directory recursively. If we
                // could figure out which files are imported by our protos we
                // could specify only those files instead.
                println!("cargo:rerun-if-changed={}", path.as_ref().display())
            }
        }

        config.service_generator(self.service_generator());

        config.compile_protos(protos, includes)?;

        Ok(())
    }

    /// Turn the builder into a `ServiceGenerator` ready to be passed to `prost-build`s
    /// `Config::service_generator`.
    pub fn service_generator(self) -> Box<dyn prost_build::ServiceGenerator> {
        Box::new(ServiceGenerator::new(self))
    }
}
