use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use vox_core::{
    external_library::ExternalLibrary,
    host::PackageManifest,
    ids::{ArtifactId, HandleId, LibraryId, SessionId},
    opt::OptimizationLevel,
    source::SourceText,
    value::{HandleData, HandleSummary, RuntimeValue},
};

use crate::{CacheStats, HandleDataChunk, Runtime, RuntimeError, SessionState};
use crate::{OptimizationDump, OptimizationDumpKind, OptimizationSettings, OptimizationStatus};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSelector {
    Id(SessionId),
    Name(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOpenMode {
    Attach,
    Create,
    AttachOrCreate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOpenRequest {
    pub selector: Option<SessionSelector>,
    pub mode: SessionOpenMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub name: Option<String>,
    pub attached_endpoints: u64,
    pub reserved: bool,
}

pub trait RuntimeRunner: Clone + Send + Sync + 'static {
    fn open_session(&self, request: SessionOpenRequest) -> Result<SessionId, RunnerError>;

    fn close_session(&self, session: SessionId) -> Result<(), RunnerError>;

    fn list_sessions(&self) -> Result<Vec<SessionSummary>, RunnerError>;

    fn set_session_reserved(&self, session: SessionId, reserved: bool) -> Result<(), RunnerError>;

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

    fn set_session_optimization(
        &self,
        session: SessionId,
        xopt: OptimizationLevel,
        objects: &[String],
    ) -> Result<(), RunnerError>;

    fn session_optimization_status(
        &self,
        session: SessionId,
        object: Option<&str>,
    ) -> Result<Vec<OptimizationStatus>, RunnerError>;

    fn session_optimization_dump(
        &self,
        session: SessionId,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, RunnerError>;

    fn mount_library(&self, manifest: PackageManifest) -> Result<LibraryId, RunnerError>;

    fn mount_external_library(&self, library: ExternalLibrary) -> Result<LibraryId, RunnerError> {
        let manifest = library.build().map_err(RunnerError::Session)?;
        self.mount_library(manifest)
    }

    fn load_script(
        &self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<ArtifactId, RunnerError>;

    fn load_script_with_settings(
        &self,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<ArtifactId, RunnerError>;

    fn reload_script(&self, artifact_id: ArtifactId, source: SourceText)
    -> Result<(), RunnerError>;

    fn reload_script_with_settings(
        &self,
        artifact_id: ArtifactId,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<(), RunnerError>;

    fn unload_script(&self, artifact_id: ArtifactId) -> Result<bool, RunnerError>;

    fn run_script(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, RunnerError>;

    fn run_script_with_xopt(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
        xopt: Option<OptimizationLevel>,
    ) -> Result<RuntimeValue, RunnerError> {
        let _ = xopt;
        self.run_script(artifact_id, arguments)
    }

    fn retain_handle(&self, handle: HandleId) -> Result<bool, RunnerError>;

    fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, RunnerError>;

    fn read_handle_data(
        &self,
        handle: HandleId,
        offset: u64,
        max_bytes: u32,
    ) -> Result<HandleDataChunk, RunnerError>;

    fn get_handle_data(&self, handle: HandleId) -> Result<HandleData, RunnerError>;

    fn release_handle(&self, handle: HandleId) -> Result<bool, RunnerError>;

    fn live_handles(&self) -> Result<Vec<HandleId>, RunnerError>;

    fn package_manifests(&self) -> Result<Vec<PackageManifest>, RunnerError>;

    fn set_default_xopt(&self, xopt: OptimizationLevel) -> Result<(), RunnerError>;

    fn optimization_status(
        &self,
        artifact_id: ArtifactId,
        settings: &OptimizationSettings,
    ) -> Result<Vec<OptimizationStatus>, RunnerError>;

    fn optimization_dump(
        &self,
        artifact_id: ArtifactId,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, RunnerError>;

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
    sessions: BTreeMap<SessionId, SessionEntry>,
    named_sessions: BTreeMap<String, SessionId>,
    next_session_id: u64,
}

#[derive(Debug)]
struct SessionEntry {
    state: SessionState<EmbeddedRunner>,
    name: Option<String>,
    attached_endpoints: u64,
    reserved: bool,
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
        action: impl FnOnce(&mut SessionEntry) -> Result<T, RunnerError>,
    ) -> Result<T, RunnerError> {
        let mut sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        let session = sessions.sessions.get_mut(&session_id).ok_or_else(|| {
            RunnerError::Session(format!(
                "interactive session {} was not found",
                session_id.0
            ))
        })?;
        action(session)
    }

    fn allocate_session_id(sessions: &mut SessionRegistry) -> SessionId {
        sessions.next_session_id += 1;
        SessionId(sessions.next_session_id)
    }

    fn insert_session(&self, sessions: &mut SessionRegistry, name: Option<String>) -> SessionId {
        let session_id = Self::allocate_session_id(sessions);
        sessions.sessions.insert(
            session_id,
            SessionEntry {
                state: SessionState::new(self.clone()),
                name: name.clone(),
                attached_endpoints: 1,
                reserved: false,
            },
        );
        if let Some(name) = name {
            sessions.named_sessions.insert(name, session_id);
        }
        session_id
    }

    fn trim_session_name(name: &str) -> Result<&str, RunnerError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(RunnerError::Session(
                "interactive session name must not be empty".to_owned(),
            ));
        }
        Ok(trimmed)
    }

    fn recycle_session_if_unused(
        sessions: &mut SessionRegistry,
        session_id: SessionId,
    ) -> Result<(), RunnerError> {
        let should_recycle = sessions
            .sessions
            .get(&session_id)
            .map(|entry| entry.attached_endpoints == 0 && !entry.reserved)
            .unwrap_or(false);
        if !should_recycle {
            return Ok(());
        }

        let Some(entry) = sessions.sessions.remove(&session_id) else {
            return Ok(());
        };
        if let Some(name) = entry.name.as_ref() {
            sessions.named_sessions.remove(name);
        }
        Ok(())
    }
}

impl RuntimeRunner for EmbeddedRunner {
    fn open_session(&self, request: SessionOpenRequest) -> Result<SessionId, RunnerError> {
        let mut sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        match request.selector {
            None => match request.mode {
                SessionOpenMode::Create | SessionOpenMode::AttachOrCreate => {
                    Ok(self.insert_session(&mut sessions, None))
                }
                SessionOpenMode::Attach => Err(RunnerError::Session(
                    "anonymous sessions cannot be reopened without an id".to_owned(),
                )),
            },
            Some(SessionSelector::Id(session_id)) => {
                let entry = sessions.sessions.get_mut(&session_id).ok_or_else(|| {
                    RunnerError::Session(format!(
                        "interactive session {} was not found",
                        session_id.0
                    ))
                })?;
                entry.attached_endpoints += 1;
                Ok(session_id)
            }
            Some(SessionSelector::Name(name)) => {
                let trimmed = Self::trim_session_name(&name)?;
                if let Some(&session_id) = sessions.named_sessions.get(trimmed) {
                    if matches!(request.mode, SessionOpenMode::Create) {
                        return Err(RunnerError::Session(format!(
                            "interactive session `{trimmed}` already exists"
                        )));
                    }
                    let entry = sessions.sessions.get_mut(&session_id).ok_or_else(|| {
                        RunnerError::Session(format!(
                            "interactive session `{trimmed}` was not found"
                        ))
                    })?;
                    entry.attached_endpoints += 1;
                    Ok(session_id)
                } else if matches!(
                    request.mode,
                    SessionOpenMode::Create | SessionOpenMode::AttachOrCreate
                ) {
                    Ok(self.insert_session(&mut sessions, Some(trimmed.to_owned())))
                } else {
                    Err(RunnerError::Session(format!(
                        "interactive session `{trimmed}` was not found"
                    )))
                }
            }
        }
    }

    fn close_session(&self, session: SessionId) -> Result<(), RunnerError> {
        let mut sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        let entry = sessions.sessions.get_mut(&session).ok_or_else(|| {
            RunnerError::Session(format!("interactive session {} was not found", session.0))
        })?;
        if entry.attached_endpoints == 0 {
            return Err(RunnerError::Session(format!(
                "interactive session {} has no attached endpoints",
                session.0
            )));
        }
        entry.attached_endpoints -= 1;
        Self::recycle_session_if_unused(&mut sessions, session)
    }

    fn list_sessions(&self) -> Result<Vec<SessionSummary>, RunnerError> {
        let sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        Ok(sessions
            .sessions
            .iter()
            .map(|(id, entry)| SessionSummary {
                id: *id,
                name: entry.name.clone(),
                attached_endpoints: entry.attached_endpoints,
                reserved: entry.reserved,
            })
            .collect())
    }

    fn set_session_reserved(&self, session: SessionId, reserved: bool) -> Result<(), RunnerError> {
        let mut sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        let entry = sessions.sessions.get_mut(&session).ok_or_else(|| {
            RunnerError::Session(format!("interactive session {} was not found", session.0))
        })?;
        entry.reserved = reserved;
        Self::recycle_session_if_unused(&mut sessions, session)
    }

    fn evaluate_session_submission(
        &self,
        session: SessionId,
        raw: &str,
    ) -> Result<Option<RuntimeValue>, RunnerError> {
        self.with_session(session, |state| {
            state.state.eval(raw).map_err(map_session_error)
        })
    }

    fn run_session_script_text(
        &self,
        session: SessionId,
        path: &str,
        raw: &str,
    ) -> Result<RuntimeValue, RunnerError> {
        self.with_session(session, |state| {
            state
                .state
                .run_script_text(path, raw)
                .map_err(map_session_error)
        })
    }

    fn drop_session_item(&self, session: SessionId, raw: &str) -> Result<bool, RunnerError> {
        self.with_session(session, |state| {
            state.state.drop_item(raw).map_err(map_session_error)
        })
    }

    fn reset_session(&self, session: SessionId) -> Result<(), RunnerError> {
        self.with_session(session, |state| {
            state.state.reset().map_err(map_session_error)
        })
    }

    fn snapshot_session_source(&self, session: SessionId) -> Result<String, RunnerError> {
        self.with_session(session, |state| Ok(state.state.snapshot_source()))
    }

    fn restore_session_snapshot(
        &self,
        session: SessionId,
        label: &str,
        text: &str,
    ) -> Result<(), RunnerError> {
        self.with_session(session, |state| {
            state
                .state
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
                .state
                .set_default_xopt(xopt)
                .map_err(map_session_error)
        })
    }

    fn set_session_optimization(
        &self,
        session: SessionId,
        xopt: OptimizationLevel,
        objects: &[String],
    ) -> Result<(), RunnerError> {
        self.with_session(session, |state| {
            state
                .state
                .set_optimization(xopt, objects)
                .map_err(map_session_error)
        })
    }

    fn session_optimization_status(
        &self,
        session: SessionId,
        object: Option<&str>,
    ) -> Result<Vec<OptimizationStatus>, RunnerError> {
        self.with_session(session, |state| {
            state
                .state
                .optimization_status(object)
                .map_err(map_session_error)
        })
    }

    fn session_optimization_dump(
        &self,
        session: SessionId,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, RunnerError> {
        self.with_session(session, |state| {
            state
                .state
                .optimization_dump(object, kind)
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

    fn load_script_with_settings(
        &self,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<ArtifactId, RunnerError> {
        self.with_runtime(|runtime| {
            runtime
                .load_script_with_settings(source, settings)
                .map_err(Into::into)
        })
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

    fn reload_script_with_settings(
        &self,
        artifact_id: ArtifactId,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<(), RunnerError> {
        self.with_runtime(|runtime| {
            runtime
                .reload_script_with_settings(artifact_id, source, settings)
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
        self.run_script_with_xopt(artifact_id, arguments, None)
    }

    fn run_script_with_xopt(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
        xopt: Option<OptimizationLevel>,
    ) -> Result<RuntimeValue, RunnerError> {
        self.with_runtime(|runtime| {
            runtime
                .run_script_with_xopt(artifact_id, arguments, xopt)
                .map_err(Into::into)
        })
    }

    fn retain_handle(&self, handle: HandleId) -> Result<bool, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.retain_handle(handle)))
    }

    fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, RunnerError> {
        self.with_runtime(|runtime| Ok(runtime.describe_handle(handle)))
    }

    fn read_handle_data(
        &self,
        handle: HandleId,
        offset: u64,
        max_bytes: u32,
    ) -> Result<HandleDataChunk, RunnerError> {
        if max_bytes == 0 {
            return Err(RunnerError::Protocol(
                "handle data chunk size must be greater than zero".to_owned(),
            ));
        }
        self.with_runtime(|runtime| {
            let Some(_metadata) = runtime.handle_metadata(handle) else {
                return Err(RunnerError::Protocol(format!(
                    "unknown handle {}",
                    handle.0
                )));
            };
            let Some(bytes) = runtime.handle_data(handle) else {
                return Err(RunnerError::Protocol(format!(
                    "handle {} does not expose serializable data",
                    handle.0
                )));
            };
            let total_bytes = bytes.len() as u64;
            if offset > total_bytes {
                return Err(RunnerError::Protocol(format!(
                    "handle {} offset {} exceeds total bytes {}",
                    handle.0, offset, total_bytes
                )));
            }

            let end = offset.saturating_add(max_bytes as u64).min(total_bytes) as usize;
            Ok(HandleDataChunk {
                offset,
                total_bytes,
                bytes: bytes[offset as usize..end].to_vec(),
            })
        })
    }

    fn get_handle_data(&self, handle: HandleId) -> Result<HandleData, RunnerError> {
        let bytes = self.with_runtime(|runtime| {
            let Some(_metadata) = runtime.handle_metadata(handle) else {
                return Err(RunnerError::Protocol(format!(
                    "unknown handle {}",
                    handle.0
                )));
            };
            let Some(bytes) = runtime.handle_data(handle) else {
                return Err(RunnerError::Protocol(format!(
                    "handle {} does not expose serializable data",
                    handle.0
                )));
            };
            Ok(bytes.to_vec())
        })?;
        decode_handle_data_bytes(&bytes)
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

    fn optimization_status(
        &self,
        artifact_id: ArtifactId,
        settings: &OptimizationSettings,
    ) -> Result<Vec<OptimizationStatus>, RunnerError> {
        self.with_runtime(|runtime| {
            runtime
                .optimization_statuses(artifact_id, settings)
                .map_err(Into::into)
        })
    }

    fn optimization_dump(
        &self,
        artifact_id: ArtifactId,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, RunnerError> {
        self.with_runtime(|runtime| {
            runtime
                .optimization_dump(artifact_id, object, kind)
                .map_err(Into::into)
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

fn decode_handle_data_bytes(bytes: &[u8]) -> Result<HandleData, RunnerError> {
    let mut reader = crate::protocol::PayloadReader::new(bytes);
    let value = crate::protocol::decode_handle_data(&mut reader)
        .map_err(|error| RunnerError::Protocol(error.to_string()))?;
    reader
        .finish()
        .map_err(|error| RunnerError::Protocol(error.to_string()))?;
    Ok(value)
}
