#[derive(Debug)]
pub enum DecompressionError {
    NotFound {
        requested: String,
        known: Vec<String>,
    },
    NoCompression,
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
            DecompressionError::NoCompression => {
                write!(f, "Compressed flag set with identity or empty encoding")
            }
        }
    }
}

impl std::error::Error for DecompressionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self {
            DecompressionError::NoCompression => None,
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

#[derive(Debug)]
pub(crate) enum CompressionError {
    NoCompression,
    Failed(std::io::Error),
}

impl std::fmt::Display for CompressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            CompressionError::Failed(error) => write!(f, "Compression failed: {}", error),
            CompressionError::NoCompression => {
                write!(f, "Compression attempted without being configured")
            }
        }
    }
}

impl std::error::Error for CompressionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self {
            CompressionError::NoCompression { .. } => None,
            CompressionError::Failed(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for CompressionError {
    fn from(error: std::io::Error) -> Self {
        CompressionError::Failed(error)
    }
}
