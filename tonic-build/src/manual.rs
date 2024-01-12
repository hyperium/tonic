//! This module provides utilities for generating `tonic` service stubs and clients
//! purely in Rust without the need of `proto` files. It also enables you to set a custom `Codec`
//! if you want to use a custom serialization format other than `protobuf`.
//!
//! # Example
//!
//! ```rust,no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let greeter_service = tonic_build::manual::Service::builder()
//!         .name("Greeter")
//!         .package("helloworld")
//!         .method(
//!             tonic_build::manual::Method::builder()
//!                 .name("say_hello")
//!                 .route_name("SayHello")
//!                 // Provide the path to the Request type
//!                 .input_type("crate::HelloRequest")
//!                 // Provide the path to the Response type
//!                 .output_type("super::HelloResponse")
//!                 // Provide the path to the Codec to use
//!                 .codec_path("crate::JsonCodec")
//!                 .build(),
//!         )
//!         .build();
//!
//!     tonic_build::manual::Builder::new().compile(&[greeter_service]);
//!     Ok(())
//! }
//! ```

use crate::code_gen::CodeGenBuilder;

use proc_macro2::TokenStream;
use quote::ToTokens;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Service builder.
///
/// This builder can be used to manually define a gRPC service in rust code without the use of a
/// .proto file.
///
/// # Example
///
/// ```
/// # use tonic_build::manual::Service;
/// let greeter_service = Service::builder()
///     .name("Greeter")
///     .package("helloworld")
///     // Add various methods to the service
///     // .method()
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct ServiceBuilder {
    /// The service name in Rust style.
    name: Option<String>,
    /// The package name as it appears in the .proto file.
    package: Option<String>,
    /// The service comments.
    comments: Vec<String>,
    /// The service methods.
    methods: Vec<Method>,
}

impl ServiceBuilder {
    /// Set the name for this Service.
    ///
    /// This value will be used both as the base for the generated rust types and service trait as
    /// well as part of the route for calling this service. Routes have the form:
    /// `/<package_name>.<service_name>/<method_route_name>`
    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        self.name = Some(name.as_ref().to_owned());
        self
    }

    /// Set the package this Service is part of.
    ///
    /// This value will be used as part of the route for calling this service.
    /// Routes have the form: `/<package_name>.<service_name>/<method_route_name>`
    pub fn package(mut self, package: impl AsRef<str>) -> Self {
        self.package = Some(package.as_ref().to_owned());
        self
    }

    /// Add a comment string that should be included as a doc comment for this Service.
    pub fn comment(mut self, comment: impl AsRef<str>) -> Self {
        self.comments.push(comment.as_ref().to_owned());
        self
    }

    /// Adds a Method to this Service.
    pub fn method(mut self, method: Method) -> Self {
        self.methods.push(method);
        self
    }

    /// Build a Service.
    ///
    /// Panics if `name` or `package` weren't set.
    pub fn build(self) -> Service {
        Service {
            name: self.name.unwrap(),
            comments: self.comments,
            package: self.package.unwrap(),
            methods: self.methods,
        }
    }
}

/// A service descriptor.
#[derive(Debug)]
pub struct Service {
    /// The service name in Rust style.
    name: String,
    /// The package name as it appears in the .proto file.
    package: String,
    /// The service comments.
    comments: Vec<String>,
    /// The service methods.
    methods: Vec<Method>,
}

impl Service {
    /// Create a new `ServiceBuilder`
    pub fn builder() -> ServiceBuilder {
        ServiceBuilder::default()
    }
}

impl crate::Service for Service {
    type Comment = String;

    type Method = Method;

    fn name(&self) -> &str {
        &self.name
    }

    fn package(&self) -> &str {
        &self.package
    }

    fn identifier(&self) -> &str {
        &self.name
    }

    fn methods(&self) -> &[Self::Method] {
        &self.methods
    }

    fn comment(&self) -> &[Self::Comment] {
        &self.comments
    }
}

/// A service method descriptor.
#[derive(Debug)]
pub struct Method {
    /// The name of the method in Rust style.
    name: String,
    /// The name of the method as should be used when constructing a route
    route_name: String,
    /// The method comments.
    comments: Vec<String>,
    /// The input Rust type.
    input_type: String,
    /// The output Rust type.
    output_type: String,
    /// Identifies if client streams multiple client messages.
    client_streaming: bool,
    /// Identifies if server streams multiple server messages.
    server_streaming: bool,
    /// The path to the codec to use for this method
    codec_path: String,
}

impl Method {
    /// Create a new `MethodBuilder`
    pub fn builder() -> MethodBuilder {
        MethodBuilder::default()
    }
}

impl crate::Method for Method {
    type Comment = String;

    fn name(&self) -> &str {
        &self.name
    }

    fn identifier(&self) -> &str {
        &self.route_name
    }

    fn codec_path(&self) -> String {
        self.codec_path.clone()
    }

    fn client_streaming(&self) -> bool {
        self.client_streaming
    }

    fn server_streaming(&self) -> bool {
        self.server_streaming
    }

    fn comment(&self) -> &[Self::Comment] {
        &self.comments
    }

    fn request_response_name(
        &self,
        _proto_path: &str,
        _compile_well_known_types: bool,
    ) -> (TokenStream, TokenStream) {
        let request = syn::parse_str::<syn::Path>(&self.input_type)
            .unwrap()
            .to_token_stream();
        let response = syn::parse_str::<syn::Path>(&self.output_type)
            .unwrap()
            .to_token_stream();
        (request, response)
    }
}

/// Method builder.
///
/// This builder can be used to manually define gRPC method, which can be added to a gRPC service,
/// in rust code without the use of a .proto file.
///
/// # Example
///
/// ```
/// # use tonic_build::manual::Method;
/// let say_hello_method = Method::builder()
///     .name("say_hello")
///     .route_name("SayHello")
///     // Provide the path to the Request type
///     .input_type("crate::common::HelloRequest")
///     // Provide the path to the Response type
///     .output_type("crate::common::HelloResponse")
///     // Provide the path to the Codec to use
///     .codec_path("crate::common::JsonCodec")
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct MethodBuilder {
    /// The name of the method in Rust style.
    name: Option<String>,
    /// The name of the method as should be used when constructing a route
    route_name: Option<String>,
    /// The method comments.
    comments: Vec<String>,
    /// The input Rust type.
    input_type: Option<String>,
    /// The output Rust type.
    output_type: Option<String>,
    /// Identifies if client streams multiple client messages.
    client_streaming: bool,
    /// Identifies if server streams multiple server messages.
    server_streaming: bool,
    /// The path to the codec to use for this method
    codec_path: Option<String>,
}

impl MethodBuilder {
    /// Set the name for this Method.
    ///
    /// This value will be used for generating the client functions for calling this Method.
    ///
    /// Generally this is formatted in snake_case.
    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        self.name = Some(name.as_ref().to_owned());
        self
    }

    /// Set the route_name for this Method.
    ///
    /// This value will be used as part of the route for calling this method.
    /// Routes have the form: `/<package_name>.<service_name>/<method_route_name>`
    ///
    /// Generally this is formatted in PascalCase.
    pub fn route_name(mut self, route_name: impl AsRef<str>) -> Self {
        self.route_name = Some(route_name.as_ref().to_owned());
        self
    }

    /// Add a comment string that should be included as a doc comment for this Method.
    pub fn comment(mut self, comment: impl AsRef<str>) -> Self {
        self.comments.push(comment.as_ref().to_owned());
        self
    }

    /// Set the path to the Rust type that should be use for the Request type of this method.
    pub fn input_type(mut self, input_type: impl AsRef<str>) -> Self {
        self.input_type = Some(input_type.as_ref().to_owned());
        self
    }

    /// Set the path to the Rust type that should be use for the Response type of this method.
    pub fn output_type(mut self, output_type: impl AsRef<str>) -> Self {
        self.output_type = Some(output_type.as_ref().to_owned());
        self
    }

    /// Set the path to the Rust type that should be used as the `Codec` for this method.
    ///
    /// Currently the codegen assumes that this type implements `Default`.
    pub fn codec_path(mut self, codec_path: impl AsRef<str>) -> Self {
        self.codec_path = Some(codec_path.as_ref().to_owned());
        self
    }

    /// Sets if the Method request from the client is streamed.
    pub fn client_streaming(mut self) -> Self {
        self.client_streaming = true;
        self
    }

    /// Sets if the Method response from the server is streamed.
    pub fn server_streaming(mut self) -> Self {
        self.server_streaming = true;
        self
    }

    /// Build a Method
    ///
    /// Panics if `name`, `route_name`, `input_type`, `output_type`, or `codec_path` weren't set.
    pub fn build(self) -> Method {
        Method {
            name: self.name.unwrap(),
            route_name: self.route_name.unwrap(),
            comments: self.comments,
            input_type: self.input_type.unwrap(),
            output_type: self.output_type.unwrap(),
            client_streaming: self.client_streaming,
            server_streaming: self.server_streaming,
            codec_path: self.codec_path.unwrap(),
        }
    }
}

struct ServiceGenerator {
    builder: Builder,
    clients: TokenStream,
    servers: TokenStream,
}

impl ServiceGenerator {
    fn generate(&mut self, service: &Service) {
        if self.builder.build_server {
            let server = CodeGenBuilder::new()
                .emit_package(true)
                .compile_well_known_types(false)
                .generate_server(service, "");

            self.servers.extend(server);
        }

        if self.builder.build_client {
            let client = CodeGenBuilder::new()
                .emit_package(true)
                .compile_well_known_types(false)
                .build_transport(self.builder.build_transport)
                .generate_client(service, "");

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
#[derive(Debug)]
pub struct Builder {
    build_server: bool,
    build_client: bool,
    build_transport: bool,

    out_dir: Option<PathBuf>,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            build_server: true,
            build_client: true,
            build_transport: true,
            out_dir: None,
        }
    }
}

impl Builder {
    /// Create a new Builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable gRPC client code generation.
    ///
    /// Defaults to enabling client code generation.
    pub fn build_client(mut self, enable: bool) -> Self {
        self.build_client = enable;
        self
    }

    /// Enable or disable gRPC server code generation.
    ///
    /// Defaults to enabling server code generation.
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

    /// Set the output directory to generate code to.
    ///
    /// Defaults to the `OUT_DIR` environment variable.
    pub fn out_dir(mut self, out_dir: impl AsRef<Path>) -> Self {
        self.out_dir = Some(out_dir.as_ref().to_path_buf());
        self
    }

    /// Performs code generation for the provided services.
    ///
    /// Generated services will be output into the directory specified by `out_dir`
    /// with files named `<package_name>.<service_name>.rs`.
    pub fn compile(self, services: &[Service]) {
        let out_dir = if let Some(out_dir) = self.out_dir.as_ref() {
            out_dir.clone()
        } else {
            PathBuf::from(std::env::var("OUT_DIR").unwrap())
        };

        let mut generator = ServiceGenerator {
            builder: self,
            clients: TokenStream::default(),
            servers: TokenStream::default(),
        };

        for service in services {
            generator.generate(service);
            let mut output = String::new();
            generator.finalize(&mut output);

            let out_file = out_dir.join(format!("{}.{}.rs", service.package, service.name));
            fs::write(out_file, output).unwrap();
        }
    }
}
