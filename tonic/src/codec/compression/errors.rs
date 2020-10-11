#[derive(Debug)]
pub enum DecompressionError {
    NotFound {
        requested: String,
        known: Vec<String>,
    },
    Failed(std::io::Error),
}

impl std::fmt::Display for DecompressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            DecompressionError::NotFound { requested, known } => {
                let known_joined = known.join(", ");
                write!(
                    f,
                    "Compressor for '{}' not found. Known compressors: {}",
                    requested, known_joined
                )
            }
            DecompressionError::Failed(error) => write!(f, "Decompression failed: {}", error),
        }
    }
}

impl std::error::Error for DecompressionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self {
            DecompressionError::NotFound { .. } => None,
            DecompressionError::Failed(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for DecompressionError {
    fn from(error: std::io::Error) -> Self {
        DecompressionError::Failed(error)
    }
}
