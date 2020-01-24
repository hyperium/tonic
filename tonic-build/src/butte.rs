use super::{client, schema, server};
use butte_build::codegen::RpcGenerator;
use butte_build::ir::types::{Rpc, RpcMethod, RpcStreaming};
use proc_macro2::TokenStream;
use quote::ToTokens;
use std::path::{Path, PathBuf};

impl<'a> schema::Commentable<'a> for Rpc<'a> {
    type Comment = &'a str;
    type CommentContainer = &'a Vec<Self::Comment>;

    fn comment(&'a self) -> Self::CommentContainer {
        &self.doc.lines
    }
}

/// Context data used while generate prost service
#[derive(Debug)]
pub struct ButteContext {
    /// relative path to flatbuffers definitions from service definitions
    pub butte_path: String,
}

impl schema::Context for ButteContext {
    fn codec_name(&self) -> &str {
        "tonic::codec::ButteCodec"
    }
}

impl<'a> schema::Service<'a> for Rpc<'a> {
    type Method = RpcMethod<'a>;
    type MethodContainer = &'a Vec<Self::Method>;
    type Context = ButteContext;

    fn name(&self) -> &str {
        &self.ident.simple().as_ref()
    }

    fn package(&self) -> &str {
        "" // Cannot return a str for this
    }

    fn identifier(&self) -> &str {
        &self.ident.simple().as_ref()
    }

    fn methods(&'a self) -> Self::MethodContainer {
        &self.methods
    }
}

impl<'a> schema::Commentable<'a> for RpcMethod<'a> {
    type Comment = &'a str;
    type CommentContainer = &'a Vec<Self::Comment>;

    fn comment(&'a self) -> Self::CommentContainer {
        &self.doc.lines
    }
}

impl<'a> schema::Method<'a> for RpcMethod<'a> {
    type Context = ButteContext;

    fn name(&self) -> &str {
        &self.snake_ident.as_ref()
    }

    fn identifier(&self) -> &str {
        &self.ident.as_ref()
    }

    fn client_streaming(&self) -> bool {
        match self.metadata.streaming {
            RpcStreaming::Client | RpcStreaming::Bidi => true,
            _ => false,
        }
    }

    fn server_streaming(&self) -> bool {
        match self.metadata.streaming {
            RpcStreaming::Server | RpcStreaming::Bidi => true,
            _ => false,
        }
    }

    fn request_response_name(&self, context: &Self::Context) -> (TokenStream, TokenStream) {
        let request = syn::parse_str::<syn::Path>(&format!(
            "{}::{}<::bytes::Bytes>",
            context.butte_path, self.request_type
        ))
        .unwrap()
        .to_token_stream();

        let response = syn::parse_str::<syn::Path>(&format!(
            "{}::{}<::bytes::Bytes>",
            context.butte_path, self.response_type
        ))
        .unwrap()
        .to_token_stream();

        (request, response)
    }
}

/// Service generator builder.
#[derive(Debug, Clone)]
pub struct Builder {
    pub(crate) build_client: bool,
    pub(crate) build_server: bool,

    out_dir: Option<PathBuf>,
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

    /// Compile the .fbs files and execute code generation.
    pub fn compile<P: AsRef<Path>>(self, fbs: &[P]) -> Result<(), Box<dyn std::error::Error>> {
        let out_dir = if let Some(out_dir) = self.out_dir.as_ref() {
            out_dir.clone()
        } else {
            PathBuf::from(std::env::var("OUT_DIR").unwrap())
        };
        let ugly = !self.format;

        let rpc_generator = Box::new(ServiceGenerator::new(self));
        // TODO butte currently supports only one file at a time
        let path_ref = fbs[0].as_ref();
        let output_path = out_dir.join(
            path_ref
                .with_extension("rs")
                .file_name()
                .expect("path has no file_name"),
        );

        butte_build::compile_fbs_generic(
            ugly,
            Some(rpc_generator),
            Box::new(std::fs::File::open(path_ref)?),
            Box::new(std::fs::File::create(output_path)?),
        )?;

        Ok(())
    }
}

struct ServiceGenerator {
    builder: Builder,
}

impl ServiceGenerator {
    fn new(builder: Builder) -> Self {
        ServiceGenerator { builder }
    }
}

impl RpcGenerator for ServiceGenerator {
    fn generate<'a>(&mut self, rpc: &Rpc<'a>, token_stream: &mut TokenStream) {
        let context = ButteContext {
            butte_path: String::from("super"),
        };

        if self.builder.build_server {
            token_stream.extend(server::generate(rpc, &context));
        }

        if self.builder.build_client {
            token_stream.extend(client::generate(rpc, &context));
        }
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
        #[cfg(feature = "rustfmt")]
        format: true,
        #[cfg(not(feature = "rustfmt"))]
        format: false,
    }
}

/// Simple `.fbs` compiling.
///
/// The include directory will be the parent folder of the specified path.
/// The package name will be the filename without the extension.
pub fn compile_fbs(fbs_path: impl AsRef<Path>) -> Result<(), Box<dyn std::error::Error>> {
    let fbs_path: &Path = fbs_path.as_ref();

    // directory the main .fbs file resides in
    fbs_path
        .parent()
        .expect("flatbuffers file should reside in a directory");

    self::configure().compile(&[fbs_path])?;

    Ok(())
}
