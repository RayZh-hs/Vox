use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

mod analysis;
mod artifact_store;
mod handles;
pub mod host_exports;
mod interpreter;
mod mir_executor;
mod protocol;
mod remote;
mod runner;
mod server;
mod session;

use thiserror::Error;
use vox_compiler::{CompileRequest, Compiler};
use vox_core::{
    external_library::ExternalLibrary,
    host::{HostRegistry, PackageManifest},
    ids::{ArtifactId, HandleId, LibraryId},
    opt::{OptimizationLevel, OptimizationRank, OptimizationSubject},
    plan::CompiledArtifact,
    source::{ModuleKind, ModulePath, SourceText},
    value::{HandleData, HandleSummary, RuntimeValue},
};

pub use analysis::{
    BindingSummary, CallableParameterSummary, FunctionSummary, GenericParameterSummary,
    RecordFieldType, ReplType, TypeEnvironment, extend_manifest_symbols, infer_environment,
    language_keywords,
};
pub use artifact_store::ArtifactStore;
pub use handles::{
    GenericFunctionHandleSummary, GenericFunctionKey, GenericParameterHandleSummary,
    HandleMetadata, HandleStore, RealizationKey, RealizedFunctionHandleSummary,
};
use interpreter::Interpreter;
use mir_executor::MirExecutor;
pub use protocol::CURRENT_PROTOCOL_VERSION;
pub use remote::RemoteRunner;
pub use runner::{
    EmbeddedRunner, RunnerError, RuntimeRunner, SessionOpenMode, SessionOpenRequest,
    SessionSelector, SessionSummary,
};
pub use server::{RuntimeServer, RuntimeServerError};
pub(crate) use session::SessionState;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleDataChunk {
    pub offset: u64,
    pub total_bytes: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationSettings {
    pub default: OptimizationLevel,
    pub overrides: BTreeMap<String, OptimizationLevel>,
}

impl OptimizationSettings {
    pub fn new(default: OptimizationLevel) -> Self {
        Self {
            default,
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationStatus {
    pub object: String,
    pub requested: OptimizationLevel,
    pub rank: Option<OptimizationRank>,
    pub artifact: Option<ArtifactId>,
    pub mir_available: bool,
    pub wasm_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationDumpKind {
    Mir,
    Wasm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationDump {
    pub object: String,
    pub kind: OptimizationDumpKind,
    pub text: String,
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

#[derive(Debug, Clone, PartialEq)]
pub struct HostCallArgument {
    pub name: String,
    pub value: Option<RuntimeValue>,
}

pub type HostFunctionHandler =
    Arc<dyn Fn(&mut Runtime, &[HostCallArgument]) -> Result<RuntimeValue, String> + Send + Sync>;

#[derive(Debug, Default)]
pub struct Runtime {
    compiler: Compiler,
    host: HostRegistry,
    host_functions: BTreeMap<(ModulePath, String), RegisteredHostFunction>,
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

#[derive(Clone)]
struct RegisteredHostFunction {
    handler: HostFunctionHandler,
}

impl fmt::Debug for RegisteredHostFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<host function>")
    }
}

impl Runtime {
    pub fn mount_library(&mut self, manifest: PackageManifest) -> LibraryId {
        self.host_functions
            .retain(|(package, _), _| package != &manifest.package);
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

    pub fn mount_external_library(
        &mut self,
        library: ExternalLibrary,
    ) -> Result<LibraryId, String> {
        library.build().map(|manifest| self.mount_library(manifest))
    }

    pub fn mount_host_library<I>(
        &mut self,
        manifest: PackageManifest,
        functions: I,
    ) -> Result<LibraryId, String>
    where
        I: IntoIterator<Item = (String, HostFunctionHandler)>,
    {
        let package = manifest.package.clone();
        let declared = manifest
            .functions
            .iter()
            .map(|function| function.name.clone())
            .collect::<BTreeSet<_>>();
        let functions = functions.into_iter().collect::<Vec<_>>();
        let mut provided = BTreeSet::new();
        for (name, _) in &functions {
            if !declared.contains(name) {
                return Err(format!(
                    "host function `{}` is not declared in mounted manifest",
                    qualified_host_name(&package, name)
                ));
            }
            if !provided.insert(name.clone()) {
                return Err(format!(
                    "duplicate host function implementation for `{}`",
                    qualified_host_name(&package, name)
                ));
            }
        }

        let id = self.mount_library(manifest);
        for (name, handler) in functions {
            self.host_functions
                .insert((package.clone(), name), RegisteredHostFunction { handler });
        }
        Ok(id)
    }

    pub fn mount_registered_host_library(
        &mut self,
        manifest: PackageManifest,
    ) -> Result<LibraryId, String> {
        host_exports::mount_registered_host_library(self, manifest)
    }

    pub fn register_host_function(
        &mut self,
        package: &ModulePath,
        function: impl Into<String>,
        handler: HostFunctionHandler,
    ) -> Result<(), String> {
        let function = function.into();
        let Some(manifest) = self.host.package(package) else {
            return Err(format!("package `{}` is not mounted", package.as_str()));
        };
        if !manifest.functions.iter().any(|item| item.name == function) {
            return Err(format!(
                "host function `{}` is not declared in mounted manifest",
                qualified_host_name(package, &function)
            ));
        }

        self.host_functions.insert(
            (package.clone(), function),
            RegisteredHostFunction { handler },
        );
        Ok(())
    }

    pub fn load_script(
        &mut self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<ArtifactId, RuntimeError> {
        let request = CompileRequest {
            source,
            optimization: xopt.unwrap_or(self.default_xopt),
            optimization_overrides: BTreeMap::new(),
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
        self.reload_script_with_xopt(artifact_id, source, self.default_xopt)
    }

    pub fn reload_script_with_xopt(
        &mut self,
        artifact_id: ArtifactId,
        source: SourceText,
        xopt: OptimizationLevel,
    ) -> Result<(), RuntimeError> {
        let Some(previous_artifact) = self.artifacts.get(artifact_id).cloned() else {
            return Err(RuntimeError::MissingArtifact(artifact_id));
        };
        let previous_treewalk = self.artifacts.treewalk(artifact_id).cloned();

        let request = CompileRequest {
            source,
            optimization: xopt,
            optimization_overrides: BTreeMap::new(),
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

    pub fn load_script_with_settings(
        &mut self,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<ArtifactId, RuntimeError> {
        let request = CompileRequest {
            source,
            optimization: settings.default,
            optimization_overrides: settings.overrides,
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

    pub fn reload_script_with_settings(
        &mut self,
        artifact_id: ArtifactId,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<(), RuntimeError> {
        let Some(previous_artifact) = self.artifacts.get(artifact_id).cloned() else {
            return Err(RuntimeError::MissingArtifact(artifact_id));
        };
        let previous_treewalk = self.artifacts.treewalk(artifact_id).cloned();

        let request = CompileRequest {
            source,
            optimization: settings.default,
            optimization_overrides: settings.overrides,
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

    pub fn optimization_statuses(
        &self,
        artifact_id: ArtifactId,
        settings: &OptimizationSettings,
    ) -> Result<Vec<OptimizationStatus>, RuntimeError> {
        let artifact = self
            .artifacts
            .get(artifact_id)
            .ok_or(RuntimeError::MissingArtifact(artifact_id))?;
        Ok(artifact_optimization_statuses(
            artifact,
            artifact_id,
            settings,
        ))
    }

    pub fn optimization_dump(
        &self,
        artifact_id: ArtifactId,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, RuntimeError> {
        let artifact = self
            .artifacts
            .get(artifact_id)
            .ok_or(RuntimeError::MissingArtifact(artifact_id))?;
        Ok(artifact_optimization_dump(artifact, object, kind))
    }

    pub fn library(&self, library_id: LibraryId) -> Option<&MountedLibrary> {
        self.libraries
            .iter()
            .find(|library| library.id == library_id)
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
        self.run_script_with_xopt(artifact_id, arguments, None)
    }

    pub fn run_script_with_xopt(
        &mut self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
        xopt: Option<OptimizationLevel>,
    ) -> Result<RuntimeValue, RuntimeError> {
        let artifact = self
            .artifacts
            .get(artifact_id)
            .cloned()
            .ok_or(RuntimeError::MissingArtifact(artifact_id))?;
        let mut artifact = artifact;
        if let Some(xopt) = xopt {
            artifact.optimization = xopt;
        }

        if !matches!(artifact.kind, ModuleKind::Script { .. }) {
            return Err(RuntimeError::NotAScript(artifact_id));
        }

        if let Some(mir) = artifact.mir.clone() {
            if let Ok(value) =
                MirExecutor::new(self, artifact.id, &mir).run_script(&artifact, arguments)
            {
                return Ok(value);
            }
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

    pub fn allocate_serializable_handle(
        &mut self,
        mut summary: HandleSummary,
        data: HandleData,
    ) -> HandleId {
        let mut payload = crate::protocol::PayloadWriter::new();
        crate::protocol::encode_handle_data(&mut payload, &data)
            .expect("serializable handle data should fit into memory");
        let payload = payload.into_inner();
        summary.bytes = Some(summary.bytes.unwrap_or(payload.len() as u64));
        self.handles.allocate_serializable_data(summary, payload)
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

    pub fn handle_data(&self, handle: HandleId) -> Option<&[u8]> {
        self.handles.serialized_data(handle)
    }

    pub fn get_handle_data(&self, handle: HandleId) -> Result<HandleData, String> {
        let Some(bytes) = self.handle_data(handle) else {
            return Err(format!(
                "handle {} does not expose serializable data",
                handle.0
            ));
        };

        let mut reader = crate::protocol::PayloadReader::new(bytes);
        let data = crate::protocol::decode_handle_data(&mut reader)
            .map_err(|error| format!("failed to decode handle {} data: {error}", handle.0))?;
        reader
            .finish()
            .map_err(|error| format!("failed to decode handle {} data: {error}", handle.0))?;
        Ok(data)
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

    pub(crate) fn package_manifest(&self, package: &ModulePath) -> Option<&PackageManifest> {
        self.host.package(package)
    }

    pub(crate) fn invoke_host_function(
        &mut self,
        package: &ModulePath,
        function: &str,
        arguments: &[HostCallArgument],
    ) -> Result<RuntimeValue, String> {
        let Some(entry) = self
            .host_functions
            .get(&(package.clone(), function.to_owned()))
            .cloned()
        else {
            return Err(format!(
                "host function implementation is not mounted for `{}`",
                qualified_host_name(package, function)
            ));
        };

        (entry.handler)(self, arguments)
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

fn qualified_host_name(package: &ModulePath, function: &str) -> String {
    format!("{}.{}", package.as_str(), function)
}

fn artifact_optimization_statuses(
    artifact: &CompiledArtifact,
    artifact_id: ArtifactId,
    settings: &OptimizationSettings,
) -> Vec<OptimizationStatus> {
    let mut statuses = Vec::new();
    statuses.push(OptimizationStatus {
        object: "module".to_owned(),
        requested: settings.default,
        rank: artifact
            .optimization_rankings
            .iter()
            .find_map(|ranking| match &ranking.subject {
                OptimizationSubject::Module => Some(ranking.rank),
                OptimizationSubject::Function(_) => None,
            }),
        artifact: Some(artifact_id),
        mir_available: artifact.mir.is_some() || artifact.plan.mir_text.is_some(),
        wasm_available: artifact.plan.wasm.is_some(),
    });

    for ranking in &artifact.optimization_rankings {
        let OptimizationSubject::Function(name) = &ranking.subject else {
            continue;
        };
        statuses.push(OptimizationStatus {
            object: name.clone(),
            requested: settings
                .overrides
                .get(name)
                .copied()
                .unwrap_or(settings.default),
            rank: Some(ranking.rank),
            artifact: Some(artifact_id),
            mir_available: artifact
                .mir
                .as_ref()
                .is_some_and(|mir| mir.bodies.iter().any(|body| body.name == *name)),
            wasm_available: false,
        });
    }

    statuses
}

fn artifact_optimization_dump(
    artifact: &CompiledArtifact,
    object: &str,
    kind: OptimizationDumpKind,
) -> Option<OptimizationDump> {
    let object = normalize_optimization_object(object);
    match kind {
        OptimizationDumpKind::Mir => {
            let text = if object == "module" {
                artifact
                    .plan
                    .mir_text
                    .clone()
                    .or_else(|| artifact.mir.as_ref().map(|mir| mir.to_text()))?
            } else {
                let mut text = String::new();
                let mir = artifact.mir.as_ref()?;
                let body = mir.bodies.iter().find(|body| body.name == object)?;
                body.write_text(&mut text).ok()?;
                text
            };
            Some(OptimizationDump { object, kind, text })
        }
        OptimizationDumpKind::Wasm => {
            if object != "module" {
                return None;
            }
            let wasm = artifact.plan.wasm.as_ref()?;
            let bytes = wasm
                .bytes
                .iter()
                .enumerate()
                .map(|(index, byte)| {
                    if index % 16 == 0 {
                        format!("\n{:08x}: {:02x}", index, byte)
                    } else {
                        format!(" {:02x}", byte)
                    }
                })
                .collect::<String>();
            Some(OptimizationDump {
                object,
                kind,
                text: format!(
                    "wasm export={} summary={} bytes={}{}",
                    wasm.entry_export,
                    wasm.summary,
                    wasm.bytes.len(),
                    bytes
                ),
            })
        }
    }
}

fn normalize_optimization_object(object: &str) -> String {
    let trimmed = object.trim();
    if trimmed.is_empty() || matches!(trimmed, "module" | ".") {
        "module".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{EmbeddedRunner, InteractiveSession, Runtime};
    use vox_core::{
        opt::OptimizationLevel,
        source::SourceText,
        value::{InlineValue, RuntimeValue},
    };

    #[test]
    fn loads_script_artifacts() {
        let mut runtime = Runtime::default();
        let source = SourceText::new("demo.vox", 1, "script voxini.demo;");
        let artifact_id = runtime
            .load_script(source, None)
            .expect("script should load");
        assert!(runtime.artifact(artifact_id).is_some());
    }

    #[test]
    fn run_script_xopt_override_preserves_behavior() {
        let mut runtime = Runtime::default();
        let source = SourceText::new(
            "demo.vox",
            1,
            r#"script voxini.opt;
param input: Int = 2;
val point = { x = input, y = input + 1, };
val moved = point.updated(x = point.y + 1);
moved.x + moved.y
"#,
        );
        let artifact_id = runtime
            .load_script(source, Some(OptimizationLevel::IOpt))
            .expect("script should load");

        for xopt in [
            OptimizationLevel::NOpt,
            OptimizationLevel::IOpt,
            OptimizationLevel::SOpt,
        ] {
            assert_runtime_int(
                runtime
                    .run_script_with_xopt(artifact_id, &[], Some(xopt))
                    .expect("script should run with optimization override"),
                7,
            );
        }
    }

    #[test]
    fn named_embedded_sessions_persist_state_and_isolate_other_sessions() {
        let runner = EmbeddedRunner::default();

        let mut author = InteractiveSession::named(runner.clone(), "shared")
            .expect("shared session should open");
        assert!(
            author
                .eval("val numbers = [40, 41, 42];")
                .expect("binding should evaluate")
                .is_none()
        );

        let closure = author
            .eval("() -> numbers[1] + 1")
            .expect("closure should evaluate")
            .expect("closure should produce a result");
        assert!(
            matches!(closure, RuntimeValue::Handle(_)),
            "closures should remain handle-backed across the session boundary"
        );
        author
            .set_reserved(true)
            .expect("shared session should be reservable");

        drop(author);

        let mut collaborator = InteractiveSession::named(runner.clone(), "shared")
            .expect("shared session should reopen");
        assert_runtime_int(
            collaborator
                .eval("$()")
                .expect("last closure should remain available")
                .expect("closure call should return a value"),
            42,
        );
        assert_runtime_int(
            collaborator
                .eval("numbers[0]")
                .expect("binding should remain available")
                .expect("binding lookup should return a value"),
            40,
        );
        assert!(
            collaborator
                .eval("val extra = numbers[2];")
                .expect("second client should mutate the shared session")
                .is_none()
        );

        let mut reopened = InteractiveSession::named(runner.clone(), "shared")
            .expect("shared session should stay durable");
        assert_runtime_int(
            reopened
                .eval("extra")
                .expect("shared mutation should persist")
                .expect("shared mutation should return a value"),
            42,
        );

        let mut isolated =
            InteractiveSession::named(runner, "isolated").expect("isolated session should open");
        let error = isolated
            .eval("numbers[1]")
            .expect_err("separate named sessions must not share bindings");
        assert!(
            error.to_string().contains("numbers"),
            "unexpected isolation error: {error}"
        );
    }

    fn assert_runtime_int(value: RuntimeValue, expected: i64) {
        match value {
            RuntimeValue::Inline(InlineValue::Int(actual)) => assert_eq!(actual, expected),
            other => panic!("expected inline int {expected}, got {other:?}"),
        }
    }
}
