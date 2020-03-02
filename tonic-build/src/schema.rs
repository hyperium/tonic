use proc_macro2::TokenStream;

/// Context data used in code generation
pub trait Context {
    /// Provide name of tonic compatibale codec
    fn codec_name(&self) -> &str;
}

/// Item has comment
pub trait Commentable<'a> {
    /// Comment type
    type Comment: AsRef<str> + 'a;
    /// Container has comments.
    type CommentContainer: IntoIterator<Item = &'a Self::Comment>;
    /// Get comments about this item
    fn comment(&'a self) -> Self::CommentContainer;
}

/// Service
pub trait Service<'a>: Commentable<'a> {
    /// Method type
    type Method: Method<'a, Context = Self::Context> + 'a;
    /// Container has methods
    type MethodContainer: IntoIterator<Item = &'a Self::Method>;
    /// Common context
    type Context: Context + 'a;

    /// Name of service
    fn name(&self) -> &str;
    /// Package name of service
    fn package(&self) -> &str;
    /// Identifier used to generate type name
    fn identifier(&self) -> &str;
    /// Methods provided by service
    fn methods(&'a self) -> Self::MethodContainer;
}

/// Method
pub trait Method<'a>: Commentable<'a> {
    /// Common context
    type Context: Context + 'a;

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
