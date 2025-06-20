/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
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
                "Scheme must not contain uppercase characters: {}",
                scheme
            ));
        }
        self.inner
            .lock()
            .unwrap()
            .insert(scheme.to_string(), Arc::from(builder));
        return Ok(());
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
