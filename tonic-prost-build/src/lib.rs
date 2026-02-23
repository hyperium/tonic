//! Prost build integration for tonic.
//!
//! This crate provides code generation for gRPC services using protobuf definitions
//! via the [`prost`] ecosystem.
//!
//! [`prost`]: https://github.com/tokio-rs/prost
//!
//! # Example
//!
//! ```rust,ignore
//! use tonic_prost_build::configure;
//!
//! fn main() {
//!     configure()
//!         .compile_protos(&["proto/service.proto"], &["proto"])
//!         .unwrap();
//! }
//! ```

#![warn(
    missing_docs,
    missing_debug_implementations,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/tokio-rs/website/master/public/img/icons/tonic.svg"
)]
#![doc(html_root_url = "https://docs.rs/tonic-prost-build/0.14.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

use proc_macro2::TokenStream;
use prost_build::{Method, Service};
use quote::{quote, ToTokens};
use std::{
    collections::HashSet,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};
use tonic_build::{Attributes, CodeGenBuilder};

#[cfg(test)]
mod tests;

// Re-export core build functionality from tonic-build
pub use tonic_build::{
    manual, Attributes as TonicAttributes, Method as TonicMethod, Service as TonicService,
};

// Re-export prost types that users might need
pub use prost_build::Config;
pub use prost_types::FileDescriptorSet;

/// Configure `tonic-prost-build` code generation.
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
        generate_default_stubs: false,
        codec_path: "tonic_prost::ProstCodec".to_string(),
        skip_debug: HashSet::default(),
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

    self::configure().compile_protos(&[proto_path], &[proto_dir])
}

/// Simple file descriptor set compiling. Use [`configure`] instead if you need more options.
pub fn compile_fds(fds: prost_types::FileDescriptorSet) -> io::Result<()> {
    self::configure().compile_fds(fds)
}

/// Non-path Rust types allowed for request/response types.
const NON_PATH_TYPE_ALLOWLIST: &[&str] = &["()"];

/// Newtype wrapper for prost to add tonic-specific extensions
struct TonicBuildService {
    prost_service: Service,
    methods: Vec<TonicBuildMethod>,
}

impl TonicBuildService {
    fn new(prost_service: Service, codec_path: String) -> Self {
        Self {
            // The tonic_build::Service trait specifies that methods are borrowed, so they have to reified up front.
            methods: prost_service
                .methods
                .iter()
                .map(|prost_method| TonicBuildMethod {
                    prost_method: prost_method.clone(),
                    codec_path: codec_path.clone(),
                })
                .collect(),
            prost_service,
        }
    }
}

/// Newtype wrapper for prost to add tonic-specific extensions
struct TonicBuildMethod {
    prost_method: Method,
    codec_path: String,
}

impl tonic_build::Service for TonicBuildService {
    type Method = TonicBuildMethod;
    type Comment = String;

    fn name(&self) -> &str {
        &self.prost_service.name
    }

    fn package(&self) -> &str {
        &self.prost_service.package
    }

    fn identifier(&self) -> &str {
        &self.prost_service.proto_name
    }

    fn methods(&self) -> &[Self::Method] {
        &self.methods
    }

    fn comment(&self) -> &[Self::Comment] {
        &self.prost_service.comments.leading
    }
}

impl tonic_build::Method for TonicBuildMethod {
    type Comment = String;

    fn name(&self) -> &str {
        &self.prost_method.name
    }

    fn identifier(&self) -> &str {
        &self.prost_method.proto_name
    }

    fn client_streaming(&self) -> bool {
        self.prost_method.client_streaming
    }

    fn server_streaming(&self) -> bool {
        self.prost_method.server_streaming
    }

    fn comment(&self) -> &[Self::Comment] {
        &self.prost_method.comments.leading
    }

    fn request_response_name(
        &self,
        proto_path: &str,
        compile_well_known_types: bool,
    ) -> (TokenStream, TokenStream) {
        let request = if is_google_type(&self.prost_method.input_type) && !compile_well_known_types
        {
            // For well-known types, map to absolute paths that will work with super::
            match self.prost_method.input_type.as_str() {
                ".google.protobuf.Empty" => quote!(()),
                ".google.protobuf.Any" => quote!(::prost_types::Any),
                ".google.protobuf.StringValue" => quote!(::prost::alloc::string::String),
                _ => {
                    // For other google types, assume they're in prost_types
                    let type_name = self
                        .prost_method
                        .input_type
                        .trim_start_matches(".google.protobuf.")
                        .to_string();
                    syn::parse_str::<syn::Path>(&format!("::prost_types::{type_name}"))
                        .unwrap()
                        .to_token_stream()
                }
            }
        } else if NON_PATH_TYPE_ALLOWLIST
            .iter()
            .any(|ty| self.prost_method.input_type.ends_with(ty))
        {
            self.prost_method.input_type.parse::<TokenStream>().unwrap()
        } else {
            // Check if this is an extern type that starts with :: or crate::
            if self.prost_method.input_type.starts_with("::")
                || self.prost_method.input_type.starts_with("crate::")
            {
                // This is an extern type, use it directly
                self.prost_method.input_type.parse::<TokenStream>().unwrap()
            } else {
                // Replace dots with double colons for the type name
                let rust_type = self.prost_method.input_type.replace('.', "::");
                // Remove leading :: if present
                let rust_type = rust_type.trim_start_matches("::");
                syn::parse_str::<syn::Path>(&format!("{proto_path}::{rust_type}"))
                    .unwrap()
                    .to_token_stream()
            }
        };

        let response =
            if is_google_type(&self.prost_method.output_type) && !compile_well_known_types {
                // For well-known types, map to absolute paths that will work with super::
                match self.prost_method.output_type.as_str() {
                    ".google.protobuf.Empty" => quote!(()),
                    ".google.protobuf.Any" => quote!(::prost_types::Any),
                    ".google.protobuf.StringValue" => quote!(::prost::alloc::string::String),
                    _ => {
                        // For other google types, assume they're in prost_types
                        let type_name = self
                            .prost_method
                            .output_type
                            .trim_start_matches(".google.protobuf.")
                            .to_string();
                        syn::parse_str::<syn::Path>(&format!("::prost_types::{type_name}"))
                            .unwrap()
                            .to_token_stream()
                    }
                }
            } else if NON_PATH_TYPE_ALLOWLIST
                .iter()
                .any(|ty| self.prost_method.output_type.ends_with(ty))
            {
                self.prost_method
                    .output_type
                    .parse::<TokenStream>()
                    .unwrap()
            } else {
                // Check if this is an extern type that starts with :: or crate::
                if self.prost_method.output_type.starts_with("::")
                    || self.prost_method.output_type.starts_with("crate::")
                {
                    // This is an extern type, use it directly
                    self.prost_method
                        .output_type
                        .parse::<TokenStream>()
                        .unwrap()
                } else {
                    // Replace dots with double colons for the type name
                    let rust_type = self.prost_method.output_type.replace('.', "::");
                    // Remove leading :: if present
                    let rust_type = rust_type.trim_start_matches("::");
                    syn::parse_str::<syn::Path>(&format!("{proto_path}::{rust_type}"))
                        .unwrap()
                        .to_token_stream()
                }
            };

        (request, response)
    }

    fn codec_path(&self) -> &str {
        &self.codec_path
    }

    fn deprecated(&self) -> bool {
        self.prost_method.options.deprecated()
    }
}

fn is_google_type(ty: &str) -> bool {
    ty.starts_with(".google.protobuf")
}

/// Service generator that is compatible with prost-build
#[derive(Debug)]
struct ServiceGenerator {
    build_client: bool,
    build_server: bool,
    build_transport: bool,
    client_attributes: Attributes,
    server_attributes: Attributes,
    use_arc_self: bool,
    generate_default_stubs: bool,
    proto_path: String,
    compile_well_known_types: bool,
    codec_path: String,
    disable_comments: HashSet<String>,
}

impl ServiceGenerator {
    /// Create a new ServiceGenerator
    #[allow(clippy::too_many_arguments)]
    fn new(
        build_client: bool,
        build_server: bool,
        build_transport: bool,
        client_attributes: Attributes,
        server_attributes: Attributes,
        use_arc_self: bool,
        generate_default_stubs: bool,
        proto_path: String,
        compile_well_known_types: bool,
        codec_path: String,
        disable_comments: HashSet<String>,
    ) -> Self {
        ServiceGenerator {
            build_client,
            build_server,
            build_transport,
            client_attributes,
            server_attributes,
            use_arc_self,
            generate_default_stubs,
            proto_path,
            compile_well_known_types,
            codec_path,
            disable_comments,
        }
    }
}

impl prost_build::ServiceGenerator for ServiceGenerator {
    fn generate(&mut self, service: Service, buf: &mut String) {
        let tonic_service = TonicBuildService::new(service, self.codec_path.clone());

        let mut builder = CodeGenBuilder::new();
        builder
            .emit_package(true)
            .build_transport(self.build_transport)
            .compile_well_known_types(self.compile_well_known_types)
            .disable_comments(self.disable_comments.clone())
            .use_arc_self(self.use_arc_self)
            .generate_default_stubs(self.generate_default_stubs);

        let mut tokens = TokenStream::new();

        if self.build_client {
            builder.attributes(self.client_attributes.clone());
            let client_code = builder.generate_client(&tonic_service, &self.proto_path);
            tokens.extend(client_code);
        }

        if self.build_server {
            builder.attributes(self.server_attributes.clone());
            let server_code = builder.generate_server(&tonic_service, &self.proto_path);
            tokens.extend(server_code);
        }

        let formatted = prettyplease::unparse(&syn::parse2(tokens).unwrap());
        buf.push_str(&formatted);
    }
}

/// Builder for configuring and generating code from `.proto` files.
#[derive(Debug, Clone)]
pub struct Builder {
    build_client: bool,
    build_server: bool,
    build_transport: bool,
    file_descriptor_set_path: Option<PathBuf>,
    skip_protoc_run: bool,
    out_dir: Option<PathBuf>,
    extern_path: Vec<(String, String)>,
    field_attributes: Vec<(String, String)>,
    message_attributes: Vec<(String, String)>,
    enum_attributes: Vec<(String, String)>,
    type_attributes: Vec<(String, String)>,
    boxed: Vec<String>,
    btree_map: Option<Vec<String>>,
    bytes: Option<Vec<String>>,
    server_attributes: Attributes,
    client_attributes: Attributes,
    proto_path: String,
    compile_well_known_types: bool,
    emit_package: bool,
    protoc_args: Vec<OsString>,
    include_file: Option<PathBuf>,
    emit_rerun_if_changed: bool,
    disable_comments: HashSet<String>,
    use_arc_self: bool,
    generate_default_stubs: bool,
    codec_path: String,
    skip_debug: HashSet<String>,
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

    /// Enable or disable transport-related features.
    pub fn build_transport(mut self, enable: bool) -> Self {
        self.build_transport = enable;
        self
    }

    /// Configure the output directory where generated Rust files are written.
    ///
    /// If unset, defaults to the `OUT_DIR` environment variable. `OUT_DIR` is set by Cargo when
    /// executing build scripts, so `out_dir` typically does not need to be configured.
    pub fn out_dir(mut self, out_dir: impl AsRef<Path>) -> Self {
        self.out_dir = Some(out_dir.as_ref().to_path_buf());
        self
    }

    /// Declare externally provided Protobuf package or type.
    ///
    /// Passed directly to `prost_build::Config.extern_path`.
    /// Note that both the Protobuf path and the rust package paths should both be fully qualified.
    /// i.e. Protobuf path should start with "." and rust path should start with "::"
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

    /// Add additional attribute to matched enums and one-offs.
    ///
    /// Passed directly to `prost_build::Config.enum_attribute`.
    pub fn enum_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.enum_attributes
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

    /// Add a field that should be boxed.
    ///
    /// Passed directly to `prost_build::Config.boxed`.
    pub fn boxed<P: AsRef<str>>(mut self, path: P) -> Self {
        self.boxed.push(path.as_ref().to_string());
        self
    }

    /// Configure btree_map on a message.
    ///
    /// Passed directly to `prost_build::Config.btree_map`.
    pub fn btree_map<P: AsRef<str>>(mut self, path: P) -> Self {
        match &mut self.btree_map {
            Some(ref mut paths) => paths.push(path.as_ref().to_string()),
            None => self.btree_map = Some(vec![path.as_ref().to_string()]),
        }
        self
    }

    /// Configure bytes on a message.
    ///
    /// Passed directly to `prost_build::Config.bytes`.
    pub fn bytes<P: AsRef<str>>(mut self, path: P) -> Self {
        match &mut self.bytes {
            Some(ref mut paths) => paths.push(path.as_ref().to_string()),
            None => self.bytes = Some(vec![path.as_ref().to_string()]),
        }
        self
    }

    /// Add additional attribute to matched server `mod`s. Passed directly to
    /// `prost_build::Config.message_attribute`
    pub fn server_mod_attribute<P: AsRef<str>, A: AsRef<str>>(
        mut self,
        path: P,
        attribute: A,
    ) -> Self {
        self.server_attributes
            .push_mod(path.as_ref(), attribute.as_ref());
        self
    }

    /// Add additional attribute to matched service servers. Passed directly to
    /// `prost_build::Config.message_attribute`
    pub fn server_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.server_attributes
            .push_struct(path.as_ref(), attribute.as_ref());
        self
    }

    /// Add additional attribute to matched traits. Passed directly to
    /// `prost_build::Config.message_attribute`
    pub fn trait_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.server_attributes
            .push_trait(path.as_ref(), attribute.as_ref());
        self
    }

    /// Add additional attribute to matched client `mod`s. Passed directly to
    /// `prost_build::Config.message_attribute`
    pub fn client_mod_attribute<P: AsRef<str>, A: AsRef<str>>(
        mut self,
        path: P,
        attribute: A,
    ) -> Self {
        self.client_attributes
            .push_mod(path.as_ref(), attribute.as_ref());
        self
    }

    /// Add additional attribute to matched service clients. Passed directly to
    /// `prost_build::Config.message_attribute`
    pub fn client_attribute<P: AsRef<str>, A: AsRef<str>>(mut self, path: P, attribute: A) -> Self {
        self.client_attributes
            .push_struct(path.as_ref(), attribute.as_ref());
        self
    }

    /// Set the path to where protobuf types are generated in the module tree.
    /// Default is `super`.
    ///
    /// This should be used in combination with `extern_path` when you want to use types that are
    /// defined in other crates or modules, since types generated with `.proto_path("my_types")`
    /// will be at the path `my_types::generated_package_name`.
    ///
    /// This will change the path that is used to include the generated types, for example:
    /// - `my_package::my_type` (default `proto_path`)
    /// - `my_types::my_package::my_type` (with `proto_path("my_types")`)
    ///
    /// Use `.extern_path("my.package", "::my_types::my_package")` to use the generated types.
    pub fn proto_path(mut self, proto_path: impl AsRef<str>) -> Self {
        self.proto_path = proto_path.as_ref().to_string();
        self
    }

    /// Enable or disable directing Protobuf to compile well-known types.
    ///
    /// Passed directly to `prost_build::Config.compile_well_known_types`.
    pub fn compile_well_known_types(mut self, enable: bool) -> Self {
        self.compile_well_known_types = enable;
        self
    }

    /// Enable or disable emitting package information.
    ///
    /// Passed directly to `prost_build::Config.emit_package`.
    pub fn emit_package(mut self, enable: bool) -> Self {
        self.emit_package = enable;
        self
    }

    /// Set the output file's path, used to write the file descriptor set.
    ///
    /// Passed directly to `prost_build::Config.file_descriptor_set_path`.
    pub fn file_descriptor_set_path(mut self, path: impl AsRef<Path>) -> Self {
        self.file_descriptor_set_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Skip building protos and just generate code using the provided file descriptor set.
    ///
    /// Passed directly to `prost_build::Config.skip_protoc_run`.
    pub fn skip_protoc_run(mut self) -> Self {
        self.skip_protoc_run = true;
        self
    }

    /// Add additional protoc arguments.
    ///
    /// Passed directly to `prost_build::Config.protoc_arg`.
    pub fn protoc_arg<A: AsRef<str>>(mut self, arg: A) -> Self {
        self.protoc_args.push(arg.as_ref().into());
        self
    }

    /// Set the include file.
    ///
    /// Passed directly to `prost_build::Config.include_file`.
    pub fn include_file(mut self, path: impl AsRef<Path>) -> Self {
        self.include_file = Some(path.as_ref().to_path_buf());
        self
    }

    /// Controls the generation of `include!` statements in the output files.
    ///
    /// Passed directly to `prost_build::Config.emit_rerun_if_changed`.
    pub fn emit_rerun_if_changed(mut self, enable: bool) -> Self {
        self.emit_rerun_if_changed = enable;
        self
    }

    /// Set the comments that should be disabled.
    ///
    /// Passed directly to `prost_build::Config.disable_comments`.
    pub fn disable_comments<I, S>(mut self, path: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.disable_comments
            .extend(path.into_iter().map(|s| s.as_ref().to_string()));
        self
    }

    /// Use `Arc<Self>` on the Server trait.
    pub fn use_arc_self(mut self, enable: bool) -> Self {
        self.use_arc_self = enable;
        self
    }

    /// Generate the default stubs for gRPC services. This code will be generated
    /// inside of your service module. Ex: `pub mod helloworld { ... }`
    pub fn generate_default_stubs(mut self, enable: bool) -> Self {
        self.generate_default_stubs = enable;
        self
    }

    /// Set the codec path for generated gRPC services.
    pub fn codec_path(mut self, path: impl AsRef<str>) -> Self {
        self.codec_path = path.as_ref().to_string();
        self
    }

    /// Configure the code generator not to strip the `Debug` implementation for the request and
    /// response types from the generated code.
    ///
    /// Passed directly to `prost_build::Config.skip_debug`.
    pub fn skip_debug<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.skip_debug
            .extend(paths.into_iter().map(|s| s.as_ref().to_string()));
        self
    }

    /// Compile the .proto files and execute code generation.
    pub fn compile_protos<P>(self, protos: &[P], includes: &[P]) -> io::Result<()>
    where
        P: AsRef<Path>,
    {
        self.compile_with_config(Config::new(), protos, includes)
    }

    /// Compile the .proto files and execute code generation with a custom `prost_build::Config`.
    ///
    /// Note: When using a custom config, any disable_comments settings on the Builder will be ignored
    /// to preserve the disable_comments already configured on the provided Config.
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

        config.out_dir(&out_dir);

        for (proto_path, rust_path) in &self.extern_path {
            config.extern_path(proto_path, rust_path);
        }

        for (prost_path, attr) in &self.field_attributes {
            config.field_attribute(prost_path, attr);
        }

        for (prost_path, attr) in &self.message_attributes {
            config.message_attribute(prost_path, attr);
        }

        for (prost_path, attr) in &self.enum_attributes {
            config.enum_attribute(prost_path, attr);
        }

        for (prost_path, attr) in &self.type_attributes {
            config.type_attribute(prost_path, attr);
        }

        for prost_path in &self.boxed {
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

        for arg in &self.protoc_args {
            config.protoc_arg(arg);
        }

        if let Some(path) = &self.include_file {
            config.include_file(path);
        }

        // Note: We don't pass self.disable_comments to prost Config here
        // because those are meant for service/method paths which are handled
        // by the ServiceGenerator, not for message paths

        if !self.skip_debug.is_empty() {
            config.skip_debug(self.skip_debug.clone());
        }

        if let Some(path) = &self.file_descriptor_set_path {
            config.file_descriptor_set_path(path);
        }

        if self.skip_protoc_run {
            config.skip_protoc_run();
        }

        if self.build_client || self.build_server {
            let service_generator = ServiceGenerator::new(
                self.build_client,
                self.build_server,
                self.build_transport,
                self.client_attributes,
                self.server_attributes,
                self.use_arc_self,
                self.generate_default_stubs,
                self.proto_path,
                self.compile_well_known_types,
                self.codec_path.clone(),
                self.disable_comments,
            );

            config.service_generator(Box::new(service_generator));
        };

        config.compile_protos(protos, includes)?;

        Ok(())
    }

    /// Compile a [`prost_types::FileDescriptorSet`] and execute code generation.
    pub fn compile_fds(self, fds: prost_types::FileDescriptorSet) -> io::Result<()> {
        self.compile_fds_with_config(fds, Config::new())
    }

    /// Compile a [`prost_types::FileDescriptorSet`] with a custom `prost_build::Config`.
    pub fn compile_fds_with_config(
        self,
        fds: prost_types::FileDescriptorSet,
        mut config: Config,
    ) -> io::Result<()> {
        let out_dir = if let Some(out_dir) = self.out_dir.as_ref() {
            out_dir.clone()
        } else {
            PathBuf::from(std::env::var("OUT_DIR").unwrap())
        };

        config.out_dir(&out_dir);

        for (proto_path, rust_path) in &self.extern_path {
            config.extern_path(proto_path, rust_path);
        }

        for (prost_path, attr) in &self.field_attributes {
            config.field_attribute(prost_path, attr);
        }

        for (prost_path, attr) in &self.message_attributes {
            config.message_attribute(prost_path, attr);
        }

        for (prost_path, attr) in &self.enum_attributes {
            config.enum_attribute(prost_path, attr);
        }

        for (prost_path, attr) in &self.type_attributes {
            config.type_attribute(prost_path, attr);
        }

        for prost_path in &self.boxed {
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

        for arg in &self.protoc_args {
            config.protoc_arg(arg);
        }

        if let Some(path) = &self.include_file {
            config.include_file(path);
        }

        // Note: We don't pass self.disable_comments to prost Config here
        // because those are meant for service/method paths which are handled
        // by the ServiceGenerator, not for message paths

        if !self.skip_debug.is_empty() {
            config.skip_debug(self.skip_debug.clone());
        }

        if let Some(path) = &self.file_descriptor_set_path {
            config.file_descriptor_set_path(path);
        }

        if self.skip_protoc_run {
            config.skip_protoc_run();
        }

        if self.build_client || self.build_server {
            let service_generator = ServiceGenerator::new(
                self.build_client,
                self.build_server,
                self.build_transport,
                self.client_attributes,
                self.server_attributes,
                self.use_arc_self,
                self.generate_default_stubs,
                self.proto_path,
                self.compile_well_known_types,
                self.codec_path.clone(),
                self.disable_comments,
            );

            config.service_generator(Box::new(service_generator));
        };

        config.compile_fds(fds)?;

        Ok(())
    }

    /// Turn the builder into a `ServiceGenerator` ready to be passed to `prost-build`s
    /// `Config::service_generator`.
    pub fn service_generator(self) -> Box<dyn prost_build::ServiceGenerator> {
        Box::new(ServiceGenerator::new(
            self.build_client,
            self.build_server,
            self.build_transport,
            self.client_attributes,
            self.server_attributes,
            self.use_arc_self,
            self.generate_default_stubs,
            self.proto_path,
            self.compile_well_known_types,
            self.codec_path.clone(),
            self.disable_comments,
        ))
    }
}
