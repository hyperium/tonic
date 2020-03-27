use proc_macro2::TokenStream;

/// Item has comment
pub trait Commentable {
    /// Comment type
    type Comment: AsRef<str>;
    /// Get comments about this item
    fn comment(&self) -> &[Self::Comment];
}

/// Service
pub trait Service: Commentable {
    /// Path to the codec
    const CODEC_PATH: &'static str;

    /// Method type
    type Method: Method;

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
    /// Path to the codec
    const CODEC_PATH: &'static str;

    /// Name of method
    fn name(&self) -> &str;
    /// Identifier used to generate type name
    fn identifier(&self) -> &str;
    /// Method is streamed by client
    fn client_streaming(&self) -> bool;
    /// Method is streamed by server
    fn server_streaming(&self) -> bool;
    /// Type name of request and response
    fn request_response_name(&self, proto_path: &str) -> (TokenStream, TokenStream);
}
