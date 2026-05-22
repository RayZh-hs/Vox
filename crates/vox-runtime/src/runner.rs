use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use vox_core::{
    host::PackageManifest,
    ids::{ArtifactId, HandleId, LibraryId, SessionId},
    opt::OptimizationLevel,
    source::SourceText,
    value::{HandleSummary, RuntimeValue},
};

use crate::{CacheStats, Runtime, RuntimeError, SessionState};

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("runtime state is unavailable: {0}")]
    Unavailable(String),
    #[error("runtime protocol error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Session(String),
}

pub trait RuntimeRunner: Clone + Send + Sync + 'static {
    fn open_session(&self, name: Option<&str>) -> Result<SessionId, RunnerError>;

    fn evaluate_session_submission(
        &self,
        session: SessionId,
        raw: &str,
    ) -> Result<Option<RuntimeValue>, RunnerError>;

    fn run_session_script_text(
        &self,
        session: SessionId,
        path: &str,
        raw: &str,
    ) -> Result<RuntimeValue, RunnerError>;

    fn drop_session_item(&self, session: SessionId, raw: &str) -> Result<bool, RunnerError>;

    fn reset_session(&self, session: SessionId) -> Result<(), RunnerError>;

    fn snapshot_session_source(&self, session: SessionId) -> Result<String, RunnerError>;

    fn restore_session_snapshot(
        &self,
        session: SessionId,
        label: &str,
        text: &str,
    ) -> Result<(), RunnerError>;

    fn set_session_default_xopt(
        &self,
        session: SessionId,
        xopt: OptimizationLevel,
    ) -> Result<(), RunnerError>;

    fn mount_library(&self, manifest: PackageManifest) -> Result<LibraryId, RunnerError>;

    fn load_script(
        &self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<ArtifactId, RunnerError>;

    fn reload_script(&self, artifact_id: ArtifactId, source: SourceText)
    -> Result<(), RunnerError>;

    fn unload_script(&self, artifact_id: ArtifactId) -> Result<bool, RunnerError>;

    fn run_script(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, RunnerError>;

    fn retain_handle(&self, handle: HandleId) -> Result<bool, RunnerError>;

    fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, RunnerError>;

    fn release_handle(&self, handle: HandleId) -> Result<bool, RunnerError>;

    fn live_handles(&self) -> Result<Vec<HandleId>, RunnerError>;

    fn package_manifests(&self) -> Result<Vec<PackageManifest>, RunnerError>;

    fn set_default_xopt(&self, xopt: OptimizationLevel) -> Result<(), RunnerError>;

    fn cache_stats(&self) -> Result<CacheStats, RunnerError>;

    fn clear_artifacts(&self) -> Result<(), RunnerError>;
}

#[derive(Debug, Default)]
struct EmbeddedState {
    runtime: Mutex<Runtime>,
    sessions: Mutex<SessionRegistry>,
}

#[derive(Debug, Default)]
struct SessionRegistry {
    sessions: BTreeMap<SessionId, SessionState<EmbeddedRunner>>,
    named_sessions: BTreeMap<String, SessionId>,
    next_session_id: u64,
}

#[derive(Debug, Clone, Default)]
pub struct EmbeddedRunner {
    inner: Arc<EmbeddedState>,
}

impl EmbeddedRunner {
    pub fn new(runtime: Runtime) -> Self {
        Self {
            inner: Arc::new(EmbeddedState {
                runtime: Mutex::new(runtime),
                sessions: Mutex::new(SessionRegistry::default()),
            }),
        }
    }

    pub(crate) fn with_runtime<T>(
        &self,
        action: impl FnOnce(&mut Runtime) -> Result<T, RunnerError>,
    ) -> Result<T, RunnerError> {
        let mut runtime = self
            .inner
            .runtime
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        action(&mut runtime)
    }

    fn with_session<T>(
        &self,
        session_id: SessionId,
        action: impl FnOnce(&mut SessionState<EmbeddedRunner>) -> Result<T, RunnerError>,
    ) -> Result<T, RunnerError> {
        let mut sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        let session = sessions
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| RunnerError::Session(format!("interactive session {} was not found", session_id.0)))?;
        action(session)
    }
}

impl RuntimeRunner for EmbeddedRunner {
    fn open_session(&self, name: Option<&str>) -> Result<SessionId, RunnerError> {
        let trimmed = name.map(str::trim);
        if matches!(trimmed, Some("")) {
            return Err(RunnerError::Session(
                "interactive session name must not be empty".to_owned(),
            ));
        }

        let mut sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        if let Some(name) = trimmed {
            if let Some(&session_id) = sessions.named_sessions.get(name) {
                return Ok(session_id);
            }
        }

        sessions.next_session_id += 1;
        let session_id = SessionId(sessions.next_session_id);
        sessions
            .sessions
            .insert(session_id, SessionState::new(self.clone()));
        if let Some(name) = trimmed {
            sessions.named_sessions.insert(name.to_owned(), session_id);
        }
        Ok(session_id)
    }

    fn evaluate_session_submission(
        &self,
        session: SessionId,
        raw: &str,
    ) -> Result<Option<RuntimeValue>, RunnerError> {
        self.with_session(session, |state| {
            state
                .evaluate_submission(raw)
                .map_err(map_session_error)
        })
    }

    fn run_session_script_text(
        &self,
        session: SessionId,
        path: &str,
        raw: &str,
    ) -> Result<RuntimeValue, RunnerError> {
        self.with_session(session, |state| {
            state.run_script_text(path, raw).map_err(map_session_error)
        })
    }

    fn drop_session_item(&self, session: SessionId, raw: &str) -> Result<bool, RunnerError> {
        self.with_session(session, |state| state.drop_item(raw).map_err(map_session_error))
    }

    fn reset_session(&self, session: SessionId) -> Result<(), RunnerError> {
        self.with_session(session, |state| state.reset().map_err(map_session_error))
    }

    fn snapshot_session_source(&self, session: SessionId) -> Result<String, RunnerError> {
        self.with_session(session, |state| Ok(state.snapshot_source()))
    }

    fn restore_session_snapshot(
        &self,
        session: SessionId,
        label: &str,
        text: &str,
    ) -> Result<(), RunnerError> {
        self.with_session(session, |state| {
            state
                .restore_snapshot_source(label, text)
                .map_err(map_session_error)
        })
    }

    fn set_session_default_xopt(
        &self,
        session: SessionId,
        xopt: OptimizationLevel,
    ) -> Result<(), RunnerError> {
        self.with_session(session, |state| {
            state
                .set_default_xopt(xopt)
                .map_err(map_session_error)
        })
    }

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
        self.with_runtime(|runtime| {
            runtime
                .reload_script(artifact_id, source)
                .map_err(Into::into)
        })
    }

    fn unload_script(&self, artifact_id: ArtifactId) -> Result<bool, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.unload_script(artifact_id)))
    }

    fn run_script(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, RunnerError> {
        self.with_runtime(|runtime| {
            runtime
                .run_script(artifact_id, arguments)
                .map_err(Into::into)
        })
    }

    fn retain_handle(&self, handle: HandleId) -> Result<bool, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.retain_handle(handle)))
    }

    fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.describe_handle(handle)))
    }

    fn release_handle(&self, handle: HandleId) -> Result<bool, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.release_handle(handle)))
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

fn map_session_error(error: crate::SessionError) -> RunnerError {
    match error {
        crate::SessionError::Runner(error) => error,
        crate::SessionError::Message(message) => RunnerError::Session(message),
    }
}
