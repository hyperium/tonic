use super::{client, schema, server};
use proc_macro2::TokenStream;
use prost_build::{Config, Method, Service};
use quote::ToTokens;
use std::io;
use std::path::{Path, PathBuf};

impl<'a> schema::Commentable<'a> for Service {
    type Comment = String;
    type CommentContainer = &'a Vec<Self::Comment>;

    fn comment(&'a self) -> Self::CommentContainer {
        &self.comments.leading
    }
}

/// Context data used while generate prost service
#[derive(Debug)]
pub struct ProstContext {
    /// relative path to proto definitions from service definitions
    pub proto_path: String,
}

impl schema::Context for ProstContext {
    fn codec_name(&self) -> &str {
        "tonic::codec::ProstCodec"
    }
}

impl<'a> schema::Service<'a> for Service {
    type Method = Method;
    type MethodContainer = &'a Vec<Self::Method>;
    type Context = ProstContext;

    fn name(&self) -> &str {
        &self.name
    }

    fn package(&self) -> &str {
        &self.package
    }

    fn identifier(&self) -> &str {
        &self.proto_name
    }

    fn methods(&'a self) -> Self::MethodContainer {
        &self.methods
    }
}

impl<'a> schema::Commentable<'a> for Method {
    type Comment = String;
    type CommentContainer = &'a Vec<Self::Comment>;

    fn comment(&'a self) -> Self::CommentContainer {
        &self.comments.leading
    }
}

impl<'a> schema::Method<'a> for Method {
    type Context = ProstContext;

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

    fn request_response_name(&self, context: &Self::Context) -> (TokenStream, TokenStream) {
        let request = if self.input_proto_type.starts_with(".google.protobuf")
            || self.input_type.starts_with("::")
        {
            self.input_type.parse::<TokenStream>().unwrap()
        } else {
            syn::parse_str::<syn::Path>(&format!("{}::{}", context.proto_path, self.input_type))
                .unwrap()
                .to_token_stream()
        };

        let response = if self.output_proto_type.starts_with(".google.protobuf")
            || self.output_type.starts_with("::")
        {
            self.output_type.parse::<TokenStream>().unwrap()
        } else {
            syn::parse_str::<syn::Path>(&format!("{}::{}", context.proto_path, self.output_type))
                .unwrap()
                .to_token_stream()
        };

        (request, response)
    }
}

pub(crate) fn compile<P: AsRef<Path>>(
    builder: Builder,
    out_dir: PathBuf,
    protos: &[P],
    includes: &[P],
) -> std::io::Result<()> {
    let mut config = Config::new();

    config.out_dir(out_dir);
    for (proto_path, rust_path) in builder.extern_path.iter() {
        config.extern_path(proto_path, rust_path);
    }
    for (prost_path, attr) in builder.field_attributes.iter() {
        config.field_attribute(prost_path, attr);
    }
    for (prost_path, attr) in builder.type_attributes.iter() {
        config.type_attribute(prost_path, attr);
    }
    config.service_generator(Box::new(ServiceGenerator::new(builder)));

    config.compile_protos(protos, includes)?;

    Ok(())
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
        let context = ProstContext {
            proto_path: String::from("super"),
        };

        if self.builder.build_server {
            let server = server::generate(&service, &context);
            self.servers.extend(server);
        }

        if self.builder.build_client {
            let client = client::generate(&service, &context);
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
    pub(crate) extern_path: Vec<(String, String)>,
    pub(crate) field_attributes: Vec<(String, String)>,
    pub(crate) type_attributes: Vec<(String, String)>,

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

    /// Compile the .proto files and execute code generation.
    pub fn compile<P: AsRef<Path>>(self, protos: &[P], includes: &[P]) -> io::Result<()> {
        let out_dir = if let Some(out_dir) = self.out_dir.as_ref() {
            out_dir.clone()
        } else {
            PathBuf::from(std::env::var("OUT_DIR").unwrap())
        };

        #[cfg(feature = "rustfmt")]
        let format = self.format;

        compile(self, out_dir.clone(), protos, includes)?;

        #[cfg(feature = "rustfmt")]
        {
            if format {
                super::fmt(out_dir.to_str().expect("Expected utf8 out_dir"));
            }
        }

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
        extern_path: Vec::new(),
        field_attributes: Vec::new(),
        type_attributes: Vec::new(),
        #[cfg(feature = "rustfmt")]
        format: true,
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
