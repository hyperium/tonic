//! Compilation settings for code generation.

/// Settings used when compiling generated code.
#[derive(Debug, Clone)]
pub struct CompileSettings {
    /// The path to the codec to use for encoding/decoding messages.
    pub codec_path: String,
}

impl Default for CompileSettings {
    fn default() -> Self {
        Self {
            codec_path: "tonic_prost::ProstCodec".to_string(),
        }
    }
}
