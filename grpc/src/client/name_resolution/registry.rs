/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use super::ResolverBuilder;

static GLOBAL_RESOLVER_REGISTRY: OnceLock<ResolverRegistry> = OnceLock::new();

/// A registry to store and retrieve name resolvers.  Resolvers are indexed by
/// the URI scheme they are intended to handle.
#[derive(Default)]
pub struct ResolverRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<dyn ResolverBuilder>>>>,
}

impl ResolverRegistry {
    /// Construct an empty name resolver registry.
    fn new() -> Self {
        Self {
            inner: Arc::default(),
        }
    }

    /// Add a name resolver into the registry. builder.scheme() will
    /// be used as the scheme registered with this builder. If multiple
    /// resolvers are registered with the same name, the one registered last
    /// will take effect.
    ///
    /// # Panics
    ///
    /// Panics if the given scheme contains uppercase characters.
    pub fn add_builder(&self, builder: Box<dyn ResolverBuilder>) {
        self.try_add_builder(builder).unwrap();
    }

    /// Add a name resolver into the registry. builder.scheme() will
    /// be used as the scheme registered with this builder. If multiple
    /// resolvers are registered with the same name, the one registered last
    /// will take effect.
    pub fn try_add_builder(&self, builder: Box<dyn ResolverBuilder>) -> Result<(), String> {
        let scheme = builder.scheme();
        if scheme.chars().any(|c| c.is_ascii_uppercase()) {
            return Err(format!(
                "Scheme must not contain uppercase characters: {scheme}"
            ));
        }
        self.inner
            .lock()
            .unwrap()
            .insert(scheme.to_string(), Arc::from(builder));
        Ok(())
    }

    /// Returns the resolver builder registered for the given scheme, if any.
    ///
    /// The provided scheme is case-insensitive; any uppercase characters
    /// will be converted to lowercase before lookup.
    pub fn get(&self, scheme: &str) -> Option<Arc<dyn ResolverBuilder>> {
        self.inner
            .lock()
            .unwrap()
            .get(&scheme.to_lowercase())
            .cloned()
    }
}

/// Global registry for resolver builders.
pub fn global_registry() -> &'static ResolverRegistry {
    GLOBAL_RESOLVER_REGISTRY.get_or_init(ResolverRegistry::new)
}
