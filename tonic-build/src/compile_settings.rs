#[derive(Debug, Clone)]
pub(crate) struct CompileSettings {
    pub(crate) codec_path: String,
}

impl Default for CompileSettings {
    fn default() -> Self {
        Self {
            codec_path: "tonic::codec::ProstCodec".to_string(),
        }
    }
}
