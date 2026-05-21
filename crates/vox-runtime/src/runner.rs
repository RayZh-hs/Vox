use std::sync::{Arc, Mutex};

use thiserror::Error;
use vox_core::{
    host::PackageManifest,
    ids::{ArtifactId, HandleId, LibraryId},
    opt::OptimizationLevel,
    source::SourceText,
    value::{HandleSummary, RuntimeValue},
};

use crate::{CacheStats, Runtime, RuntimeError};

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("runtime state is unavailable: {0}")]
    Unavailable(String),
    #[error("runtime protocol error: {0}")]
    Protocol(String),
}

pub trait RuntimeRunner: Clone + Send + Sync + 'static {
    fn mount_library(&self, manifest: PackageManifest) -> Result<LibraryId, RunnerError>;

    fn load_script(
        &self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<ArtifactId, RunnerError>;

    fn reload_script(
        &self,
        artifact_id: ArtifactId,
        source: SourceText,
    ) -> Result<(), RunnerError>;

    fn unload_script(&self, artifact_id: ArtifactId) -> Result<bool, RunnerError>;

    fn run_script(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, RunnerError>;

    fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, RunnerError>;

    fn live_handles(&self) -> Result<Vec<HandleId>, RunnerError>;

    fn package_manifests(&self) -> Result<Vec<PackageManifest>, RunnerError>;

    fn set_default_xopt(&self, xopt: OptimizationLevel) -> Result<(), RunnerError>;

    fn cache_stats(&self) -> Result<CacheStats, RunnerError>;

    fn clear_artifacts(&self) -> Result<(), RunnerError>;
}

#[derive(Debug, Clone, Default)]
pub struct EmbeddedRunner {
    inner: Arc<Mutex<Runtime>>,
}

impl EmbeddedRunner {
    pub fn new(runtime: Runtime) -> Self {
        Self {
            inner: Arc::new(Mutex::new(runtime)),
        }
    }

    fn with_runtime<T>(
        &self,
        action: impl FnOnce(&mut Runtime) -> Result<T, RunnerError>,
    ) -> Result<T, RunnerError> {
        let mut runtime = self
            .inner
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        action(&mut runtime)
    }
}

impl RuntimeRunner for EmbeddedRunner {
    fn mount_library(&self, manifest: PackageManifest) -> Result<LibraryId, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.mount_library(manifest)))
    }

    fn load_script(
        &self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<ArtifactId, RunnerError> {
        self.with_runtime(|runtime| runtime.load_script(source, xopt).map_err(Into::into))
    }

    fn reload_script(
        &self,
        artifact_id: ArtifactId,
        source: SourceText,
    ) -> Result<(), RunnerError> {
        self.with_runtime(|runtime| runtime.reload_script(artifact_id, source).map_err(Into::into))
    }

    fn unload_script(&self, artifact_id: ArtifactId) -> Result<bool, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.unload_script(artifact_id)))
    }

    fn run_script(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, RunnerError> {
        self.with_runtime(|runtime| runtime.run_script(artifact_id, arguments).map_err(Into::into))
    }

    fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.describe_handle(handle).cloned()))
    }

    fn live_handles(&self) -> Result<Vec<HandleId>, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.live_handles()))
    }

    fn package_manifests(&self) -> Result<Vec<PackageManifest>, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.package_manifests()))
    }

    fn set_default_xopt(&self, xopt: OptimizationLevel) -> Result<(), RunnerError> {
        self.with_runtime(|runtime| {
            runtime.set_default_xopt(xopt);
            Ok(())
        })
    }

    fn cache_stats(&self) -> Result<CacheStats, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.cache_stats()))
    }

    fn clear_artifacts(&self) -> Result<(), RunnerError> {
        self.with_runtime(|runtime| {
            runtime.clear_artifacts();
            Ok(())
        })
    }
}
