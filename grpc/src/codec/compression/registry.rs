use super::{Compressor, Encoding};
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// A registry of compression implementations.
#[derive(Default, Clone)]
pub struct CompressionRegistry {
    codecs: HashMap<&'static str, Arc<dyn Compressor>>,
}

// The global registry using ArcSwap for lock-free reads to implement something
// closer to the RCU pattern.
static GLOBAL_REGISTRY: LazyLock<ArcSwap<CompressionRegistry>> =
    LazyLock::new(|| ArcSwap::from(Arc::new(CompressionRegistry::new())));

/// Get a codec from the global registry.
/// This operation is extremely fast and lock-free.
pub fn get_codec(name: &str) -> Option<Arc<dyn Compressor>> {
    GLOBAL_REGISTRY.load().get(name)
}

/// Add a new codec to the global registry using a copy-on-write strategy.
pub fn add_codec(name: &'static str, codec: Arc<dyn Compressor>) {
    let current_registry = GLOBAL_REGISTRY.load();
    let new_registry = current_registry.with_codec(name, codec);
    GLOBAL_REGISTRY.store(Arc::new(new_registry));
}

impl CompressionRegistry {
    /// Creates a new compression registry with default codecs enabled by features.
    pub fn new() -> Self {
        let mut codecs: HashMap<&'static str, Arc<dyn Compressor>> = HashMap::new();

        #[cfg(feature = "gzip")]
        {
            let gzip = Arc::new(super::gzip::Gzip::new());
            codecs.insert(<super::gzip::Gzip as Encoding>::NAME, gzip);
        }

        #[cfg(feature = "deflate")]
        {
            let deflate = Arc::new(super::deflate::Deflate::new());
            codecs.insert(<super::deflate::Deflate as Encoding>::NAME, deflate);
        }

        #[cfg(feature = "zstd")]
        {
            let zstd = Arc::new(super::zstd::Zstd::new());
            codecs.insert(<super::zstd::Zstd as Encoding>::NAME, zstd);
        }

        Self { codecs }
    }

    /// Get a codec from this specific registry instance.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Compressor>> {
        self.codecs.get(name).cloned()
    }

    /// Creates a new registry from an existing one, adding a new codec.
    pub fn with_codec(&self, name: &'static str, codec: Arc<dyn Compressor>) -> Self {
        // Clone the existing map of codecs
        let mut new_codecs = self.codecs.clone();
        // Add the new one
        new_codecs.insert(name, codec);
        // Return a new CompressionRegistry instance
        Self { codecs: new_codecs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::compression::{Compressor, Encoding};
    use bytes::{Buf, BufMut};
    use std::io;

    #[derive(Debug, Clone, Copy)]
    struct MockCompression;

    impl Compressor for MockCompression {
        fn compress(
            &self,
            _source: &mut dyn Buf,
            _destination: &mut dyn BufMut,
        ) -> Result<(), io::Error> {
            Ok(())
        }

        fn decompress(
            &self,
            _source: &mut dyn Buf,
            _destination: &mut dyn BufMut,
        ) -> Result<(), io::Error> {
            Ok(())
        }
    }

    impl Encoding for MockCompression {
        const NAME: &'static str = "mock";
    }

    #[test]
    fn registry_get_with() {
        let registry = CompressionRegistry::new();
        let registry = registry.with_codec("mock", Arc::new(MockCompression));
        assert!(registry.get("mock").is_some());
    }

    #[test]
    fn global_registry() {
        #[cfg(feature = "gzip")]
        assert!(get_codec("gzip").is_some());
        #[cfg(feature = "deflate")]
        assert!(get_codec("deflate").is_some());
        #[cfg(feature = "zstd")]
        assert!(get_codec("zstd").is_some());

        add_codec("mock", Arc::new(MockCompression));
        assert!(get_codec("mock").is_some());
    }
}
