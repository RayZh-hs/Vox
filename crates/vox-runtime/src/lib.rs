mod analysis;
mod artifact_store;
mod handles;
mod interpreter;
mod protocol;
mod remote;
mod runner;
mod server;
mod session;

use thiserror::Error;
use vox_compiler::{CompileRequest, Compiler};
use vox_core::{
    host::{HostRegistry, PackageManifest},
    ids::{ArtifactId, HandleId, LibraryId},
    opt::OptimizationLevel,
    plan::CompiledArtifact,
    source::{ModuleKind, SourceText},
    value::{HandleSummary, RuntimeValue},
};

pub use analysis::{
    BindingSummary, CallableParameterSummary, FunctionSummary, GenericParameterSummary,
    RecordFieldType, ReplType, TypeEnvironment, extend_manifest_symbols, infer_environment,
    language_keywords,
};
pub use artifact_store::ArtifactStore;
pub use handles::{
    GenericFunctionHandleSummary, GenericFunctionKey, GenericParameterHandleSummary, HandleStore,
    HandleMetadata, RealizationKey, RealizedFunctionHandleSummary,
};
use interpreter::Interpreter;
pub use protocol::CURRENT_PROTOCOL_VERSION;
pub use remote::RemoteRunner;
pub use runner::{EmbeddedRunner, RunnerError, RuntimeRunner};
pub use server::{RuntimeServer, RuntimeServerError};
pub use session::{InteractiveSession, SessionCompletion, SessionError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedLibrary {
    pub id: LibraryId,
    pub revision: u64,
    pub manifest: PackageManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStats {
    pub artifacts: usize,
    pub pure_cache_entries: usize,
    pub pure_cache_bytes: u64,
    pub handles: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheClearScope {
    All,
    Artifacts,
    PureCache,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("compilation failed:\n{0}")]
    CompilationFailed(String),
    #[error("artifact {0:?} was not found")]
    MissingArtifact(ArtifactId),
    #[error("artifact {0:?} is not executable as a script")]
    NotAScript(ArtifactId),
    #[error("artifact {0:?} has no executable plan yet")]
    ExecutionNotImplemented(ArtifactId),
    #[error("script execution failed: {0}")]
    ExecutionFailed(String),
}

#[derive(Debug, Default)]
pub struct Runtime {
    compiler: Compiler,
    host: HostRegistry,
    artifacts: ArtifactStore,
    handles: HandleStore,
    generic_handles: std::collections::BTreeMap<GenericFunctionKey, CachedGenericFunction>,
    libraries: Vec<MountedLibrary>,
    next_library_id: u64,
    next_library_revision: u64,
    default_xopt: OptimizationLevel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedGenericFunction {
    signature: GenericFunctionHandleSummary,
    handle: Option<HandleId>,
    realized: std::collections::BTreeMap<RealizationKey, HandleId>,
}

impl Runtime {
    pub fn mount_library(&mut self, manifest: PackageManifest) -> LibraryId {
        self.host.register_package(manifest.clone());
        self.next_library_id += 1;
        self.next_library_revision += 1;

        let id = LibraryId(self.next_library_id);
        self.libraries.push(MountedLibrary {
            id,
            revision: self.next_library_revision,
            manifest,
        });
        id
    }

    pub fn load_script(
        &mut self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<ArtifactId, RuntimeError> {
        let request = CompileRequest {
            source,
            optimization: xopt.unwrap_or(self.default_xopt),
            host: self.host.clone(),
        };
        let result = self.compiler.compile(request);

        if result.diagnostics.has_errors() {
            return Err(RuntimeError::CompilationFailed(
                result.diagnostics.to_string(),
            ));
        }

        let artifact = result
            .artifact
            .expect("successful compilation should produce an artifact");
        let treewalk = result.treewalk;
        let id = artifact.id;
        self.artifacts.insert(artifact, treewalk);
        Ok(id)
    }

    pub fn reload_script(
        &mut self,
        artifact_id: ArtifactId,
        source: SourceText,
    ) -> Result<(), RuntimeError> {
        let Some(previous_artifact) = self.artifacts.get(artifact_id).cloned() else {
            return Err(RuntimeError::MissingArtifact(artifact_id));
        };
        let previous_treewalk = self.artifacts.treewalk(artifact_id).cloned();

        let request = CompileRequest {
            source,
            optimization: self.default_xopt,
            host: self.host.clone(),
        };
        let result = self.compiler.compile(request);

        if result.diagnostics.has_errors() {
            return Err(RuntimeError::CompilationFailed(
                result.diagnostics.to_string(),
            ));
        }

        let mut artifact = result
            .artifact
            .expect("successful compilation should produce an artifact");
        artifact.id = artifact_id;
        let generic_signatures_changed = previous_artifact.module != artifact.module
            || previous_artifact.optimization != artifact.optimization
            || previous_treewalk
                .as_ref()
                .map(|treewalk| &treewalk.functions)
                != result.treewalk.as_ref().map(|treewalk| &treewalk.functions);
        self.artifacts.insert(artifact, result.treewalk);
        if generic_signatures_changed {
            self.clear_generic_handles(Some(artifact_id));
        }
        Ok(())
    }

    pub fn artifact(&self, artifact_id: ArtifactId) -> Option<&CompiledArtifact> {
        self.artifacts.get(artifact_id)
    }

    pub fn library(&self, library_id: LibraryId) -> Option<&MountedLibrary> {
        self.libraries.iter().find(|library| library.id == library_id)
    }

    pub fn unload_script(&mut self, artifact_id: ArtifactId) -> bool {
        self.clear_generic_handles(Some(artifact_id));
        self.artifacts.remove(artifact_id).is_some()
    }

    pub fn run_script(
        &mut self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, RuntimeError> {
        let artifact = self
            .artifacts
            .get(artifact_id)
            .cloned()
            .ok_or(RuntimeError::MissingArtifact(artifact_id))?;

        if !matches!(artifact.kind, ModuleKind::Script { .. }) {
            return Err(RuntimeError::NotAScript(artifact_id));
        }

        let treewalk = self
            .artifacts
            .treewalk(artifact_id)
            .ok_or(RuntimeError::ExecutionNotImplemented(artifact_id))?
            .clone();

        Interpreter::new(self, artifact.id)
            .run_script(&treewalk, &artifact, arguments)
            .map_err(RuntimeError::ExecutionFailed)
    }

    pub fn allocate_handle(&mut self, summary: HandleSummary) -> HandleId {
        self.handles.allocate_data(summary)
    }

    pub fn retain_handle(&mut self, handle: HandleId) -> bool {
        self.handles.retain(handle)
    }

    pub fn describe_handle(&self, handle: HandleId) -> Option<HandleSummary> {
        self.handles.describe(handle)
    }

    pub fn handle_metadata(&self, handle: HandleId) -> Option<HandleMetadata> {
        self.handles.metadata(handle)
    }

    pub fn release_handle(&mut self, handle: HandleId) -> bool {
        self.handles.release(handle)
    }

    pub fn live_handles(&self) -> Vec<HandleId> {
        self.handles.ids()
    }

    pub fn package_manifests(&self) -> Vec<PackageManifest> {
        self.host.packages().cloned().collect()
    }

    pub fn set_default_xopt(&mut self, xopt: OptimizationLevel) {
        self.default_xopt = xopt;
    }

    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            artifacts: self.artifacts.len(),
            pure_cache_entries: 0,
            pure_cache_bytes: 0,
            handles: self.handles.len(),
        }
    }

    pub fn clear_cache(&mut self, scope: CacheClearScope) -> u64 {
        match scope {
            CacheClearScope::All | CacheClearScope::Artifacts => {
                let cleared = self.artifacts.len() as u64;
                self.clear_artifacts();
                cleared
            }
            CacheClearScope::PureCache => 0,
        }
    }

    pub fn clear_artifacts(&mut self) {
        self.artifacts = ArtifactStore::default();
        self.clear_generic_handles(None);
    }

    pub fn materialize_generic_handle(
        &mut self,
        key: GenericFunctionKey,
        signature: GenericFunctionHandleSummary,
    ) -> HandleId {
        let cached = self
            .generic_handles
            .entry(key)
            .or_insert_with(|| CachedGenericFunction {
                signature: signature.clone(),
                handle: None,
                realized: std::collections::BTreeMap::new(),
            });
        cached.signature = signature.clone();

        if let Some(handle) = cached.handle {
            self.retain_handle(handle);
            return handle;
        }

        let handle = self
            .handles
            .allocate_generic_function(signature, cached.realized.clone());
        cached.handle = Some(handle);
        self.retain_handle(handle);
        handle
    }

    pub fn record_generic_realization(
        &mut self,
        key: GenericFunctionKey,
        signature: GenericFunctionHandleSummary,
        realization: RealizationKey,
        realized_signature: RealizedFunctionHandleSummary,
    ) {
        let cached = self
            .generic_handles
            .entry(key)
            .or_insert_with(|| CachedGenericFunction {
                signature: signature.clone(),
                handle: None,
                realized: std::collections::BTreeMap::new(),
            });
        cached.signature = signature;

        if cached.realized.contains_key(&realization) {
            return;
        }

        let realized_handle = self.handles.allocate_realized_function(realized_signature);
        cached.realized.insert(realization.clone(), realized_handle);
        if let Some(folder_handle) = cached.handle {
            self.handles.update_generic_function_realization(
                folder_handle,
                realization,
                realized_handle,
            );
        }
    }

    pub fn clear_generic_handles(&mut self, artifact: Option<ArtifactId>) {
        let keys = self
            .generic_handles
            .keys()
            .filter(|key| artifact.is_none_or(|artifact_id| key.artifact == artifact_id))
            .cloned()
            .collect::<Vec<_>>();

        for key in keys {
            if let Some(cached) = self.generic_handles.remove(&key) {
                if let Some(handle) = cached.handle {
                    self.release_handle(handle);
                }
                for handle in cached.realized.into_values() {
                    self.release_handle(handle);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Runtime;
    use vox_core::source::SourceText;

    #[test]
    fn loads_script_artifacts() {
        let mut runtime = Runtime::default();
        let source = SourceText::new("demo.vox", 1, "script voxini.demo;");
        let artifact_id = runtime
            .load_script(source, None)
            .expect("script should load");
        assert!(runtime.artifact(artifact_id).is_some());
    }
}
