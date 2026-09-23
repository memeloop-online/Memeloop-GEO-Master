//! In-memory ES module loader for the Rust-hosted worker.
//!
//! Tenant JavaScript must never reach the filesystem, the network, or the
//! package registry.  Only modules that Rust explicitly seeds here can be
//! resolved or loaded, so the worker is an authority boundary by construction
//! rather than a sandbox that has to be locked down after the fact.

use std::collections::HashMap;

use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind, resolve_import,
};

/// Serves an allow-listed, immutable set of ES modules from memory.
#[derive(Debug, Clone, Default)]
pub struct InMemoryModuleLoader {
    modules: HashMap<String, String>,
}

impl InMemoryModuleLoader {
    pub fn new(modules: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            modules: modules.into_iter().collect(),
        }
    }

    /// Builds a loader from `(specifier, source)` pairs.
    pub fn from_static(modules: &[(&str, &str)]) -> Self {
        Self::new(
            modules
                .iter()
                .map(|(specifier, source)| ((*specifier).to_owned(), (*source).to_owned())),
        )
    }

    /// The specifiers this loader is willing to serve, in insertion order.
    pub fn specifiers(&self) -> Vec<String> {
        let mut specifiers = self.modules.keys().cloned().collect::<Vec<_>>();
        specifiers.sort();
        specifiers
    }
}

impl ModuleLoader for InMemoryModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        // A seeded specifier is always resolvable; everything else is resolved
        // relative to the referrer so a bundle can be split across modules.
        if self.modules.contains_key(specifier) {
            return ModuleSpecifier::parse(specifier)
                .map_err(|error| ModuleLoaderError::generic(error.to_string()));
        }
        resolve_import(specifier, referrer)
            .map_err(|error| ModuleLoaderError::generic(error.to_string()))
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let source = self
            .modules
            .get(module_specifier.as_str())
            .cloned()
            .ok_or_else(|| {
                ModuleLoaderError::generic(format!(
                    "module `{module_specifier}` is not part of the approved bundle"
                ))
            })
            .map(|code| {
                ModuleSource::new(
                    ModuleType::JavaScript,
                    ModuleSourceCode::String(code.into()),
                    module_specifier,
                    None,
                )
            });
        ModuleLoadResponse::Sync(source)
    }
}
