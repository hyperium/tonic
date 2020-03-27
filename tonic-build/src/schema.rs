use proc_macro2::TokenStream;

/// Context data used in code generation
pub trait Context {
    /// Provide name of tonic compatibale codec
    fn codec_name(&self) -> &str;
}

/// Item has comment
pub trait Commentable {
    /// Comment type
    type Comment: AsRef<str>;
    /// Get comments about this item
    fn comment(&self) -> &[Self::Comment];
}

/// Service
pub trait Service: Commentable {
    /// Method type
    type Method: Method<Context = Self::Context>;
    /// Common context
    type Context: Context;

    /// Name of service
    fn name(&self) -> &str;
    /// Package name of service
    fn package(&self) -> &str;
    /// Identifier used to generate type name
    fn identifier(&self) -> &str;
    /// Methods provided by service
    fn methods(&self) -> &[Self::Method];
}

/// Method
pub trait Method: Commentable {
    /// Common context
    type Context: Context;

    /// Name of method
    fn name(&self) -> &str;
    /// Identifier used to generate type name
    fn identifier(&self) -> &str;
    /// Method is streamed by client
    fn client_streaming(&self) -> bool;
    /// Method is streamed by server
    fn server_streaming(&self) -> bool;
    /// Type name of request and response
    fn request_response_name(&self, context: &Self::Context) -> (TokenStream, TokenStream);
}
