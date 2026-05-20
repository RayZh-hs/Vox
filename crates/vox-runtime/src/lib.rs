mod artifact_store;
mod handles;
mod interpreter;

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

pub use artifact_store::ArtifactStore;
pub use handles::HandleStore;
use interpreter::Interpreter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedLibrary {
    pub id: LibraryId,
    pub manifest: PackageManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStats {
    pub artifacts: usize,
    pub handles: usize,
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
    libraries: Vec<MountedLibrary>,
    next_library_id: u64,
    default_xopt: OptimizationLevel,
}

impl Runtime {
    pub fn mount_library(&mut self, manifest: PackageManifest) -> LibraryId {
        self.host.register_package(manifest.clone());
        self.next_library_id += 1;

        let id = LibraryId(self.next_library_id);
        self.libraries.push(MountedLibrary { id, manifest });
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
        if self.artifacts.get(artifact_id).is_none() {
            return Err(RuntimeError::MissingArtifact(artifact_id));
        }

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
        self.artifacts.insert(artifact, result.treewalk);
        Ok(())
    }

    pub fn artifact(&self, artifact_id: ArtifactId) -> Option<&CompiledArtifact> {
        self.artifacts.get(artifact_id)
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

        Interpreter::new(self)
            .run_script(&treewalk, &artifact, arguments)
            .map_err(RuntimeError::ExecutionFailed)
    }

    pub fn allocate_handle(&mut self, summary: HandleSummary) -> HandleId {
        self.handles.allocate(summary)
    }

    pub fn describe_handle(&self, handle: HandleId) -> Option<&HandleSummary> {
        self.handles.describe(handle)
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
            handles: self.handles.len(),
        }
    }

    pub fn clear_artifacts(&mut self) {
        self.artifacts = ArtifactStore::default();
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
