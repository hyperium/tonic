#[derive(Debug, Clone)]
pub(crate) struct CompileSettings {
    #[cfg(feature = "prost")]
    pub(crate) codec_path: String,
}

impl Default for CompileSettings {
    fn default() -> Self {
        Self {
            #[cfg(feature = "prost")]
            codec_path: "tonic::codec::ProstCodec".to_string(),
        }
    }
}
