use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use vox_compiler::frontend::{
    analyze_source,
    ast::{
        Argument, AssignmentStatement, BlockExpr, BlockItem, CompilationUnit,
        CompoundAssignmentStatement, EconIntrinsic, Expr, ExprKind, ForExpr, FunctionDecl, IfExpr,
        IntrinsicExpr, LambdaExpr, LocalValueDecl, QualifiedName, RangeExpr, ReturnStatement,
        StringLiteral, StringPart, TopLevelItem, UpdatedArg, UpdatedIntrinsic, WhenArm, WhenExpr,
    },
};
use vox_core::{
    host::PackageManifest,
    ids::{ArtifactId, HandleId, SessionId},
    opt::OptimizationLevel,
    source::{ModuleKind, SourceText},
    value::{HandleData, HandleSummary, RuntimeValue},
};

use crate::{
    HandleDataChunk, OptimizationDump, OptimizationDumpKind, OptimizationSettings,
    OptimizationStatus, ReplType, RunnerError, RuntimeRunner, SessionOpenMode, SessionOpenRequest,
    SessionSelector, SessionSummary, TypeEnvironment, extend_manifest_symbols, infer_environment,
    language_keywords,
};

const REPL_MODULE: &str = "repl.session";
const LAST_VALUE_NAME: &str = "__repl_last";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCompletion {
    pub handles: Vec<String>,
    pub symbols: Vec<String>,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Runner(RunnerError),
    #[error("{0}")]
    Message(String),
}

impl From<RunnerError> for SessionError {
    fn from(error: RunnerError) -> Self {
        match error {
            RunnerError::Session(message) => Self::Message(message),
            other => Self::Runner(other),
        }
    }
}

fn compile_error(message: impl Into<String>) -> SessionError {
    let message = message.into();
    if message.starts_with("compilation failed:\n") {
        SessionError::Message(message)
    } else {
        SessionError::Message(format!("compilation failed:\n{}", message.trim()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredItem {
    key: StoredItemKey,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredItemKey {
    Import { module: String },
    Value { name: String },
    Function { name: String },
    Statement,
}

impl StoredItem {
    fn display_name(&self) -> &str {
        match &self.key {
            StoredItemKey::Import { module } => module,
            StoredItemKey::Value { name } | StoredItemKey::Function { name } => name,
            StoredItemKey::Statement => "<statement>",
        }
    }

    fn matches_drop(&self, raw: &str) -> bool {
        match &self.key {
            StoredItemKey::Import { module } => {
                module == raw
                    || module
                        .rsplit('.')
                        .next()
                        .is_some_and(|segment| segment == raw)
            }
            StoredItemKey::Value { name } | StoredItemKey::Function { name } => name == raw,
            StoredItemKey::Statement => false,
        }
    }

    fn is_hidden_last(&self) -> bool {
        matches!(&self.key, StoredItemKey::Value { name } if name == LAST_VALUE_NAME)
    }

    fn function_name(&self) -> Option<&str> {
        match &self.key {
            StoredItemKey::Function { name } => Some(name),
            _ => None,
        }
    }
}

impl StoredItemKey {
    fn conflicts_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Import { module: left }, Self::Import { module: right }) => left == right,
            (Self::Value { .. }, Self::Value { .. })
            | (Self::Value { .. }, Self::Function { .. })
            | (Self::Function { .. }, Self::Value { .. })
            | (Self::Statement, _)
            | (_, Self::Statement) => false,
            (Self::Function { name: left }, Self::Function { name: right }) => left == right,
            _ => false,
        }
    }

    fn replaces_existing(&self) -> bool {
        matches!(self, Self::Import { .. } | Self::Function { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSubmission {
    items: Vec<StoredItem>,
    result_source: Option<String>,
    identity_name: Option<String>,
    uses_last_value: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct RetainedLastValue {
    item: StoredItem,
    value: Option<RuntimeValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionModel {
    unit: CompilationUnit,
    environment: TypeEnvironment,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionState<R: RuntimeRunner> {
    runner: R,
    items: Vec<StoredItem>,
    binding_handles: BTreeMap<String, HandleId>,
    hidden_last: Option<RetainedLastValue>,
    next_source_revision: u64,
    active_artifact: Option<ArtifactId>,
    default_xopt: OptimizationLevel,
    opt_overrides: BTreeMap<String, OptimizationLevel>,
}

impl<R: RuntimeRunner> SessionState<R> {
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            items: Vec::new(),
            binding_handles: BTreeMap::new(),
            hidden_last: None,
            next_source_revision: 0,
            active_artifact: None,
            default_xopt: OptimizationLevel::IOpt,
            opt_overrides: BTreeMap::new(),
        }
    }

    pub fn eval(&mut self, raw: &str) -> Result<Option<RuntimeValue>, SessionError> {
        let parsed = parse_submission(raw)?;
        if parsed.items.is_empty() && parsed.result_source.is_none() {
            return Ok(None);
        }

        self.enforce_incremental_redefinition_policy(&parsed.items)?;

        let candidate_items = merge_items(&self.items, parsed.items.clone());
        let items_changed = candidate_items != self.items;
        let source = self.synthetic_source(
            &candidate_items,
            if parsed.uses_last_value {
                self.hidden_last_item()
            } else {
                None
            },
            parsed.result_source.as_deref(),
        );

        let frontend = analyze_source(&SourceText::new(
            "<repl-submit>",
            self.next_revision(),
            &source,
        ))
        .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;

        self.validate_environment(&frontend.syntax)?;

        let value = if parsed.result_source.is_some() {
            let value = self.evaluate_script_source(&source)?;
            if items_changed {
                self.clear_binding_handles()?;
            }
            let value = self.finalize_submission_result(
                parsed.result_source.as_deref().unwrap_or_default(),
                parsed.identity_name.as_deref(),
                value,
            )?;
            Some(value)
        } else {
            if items_changed {
                self.clear_binding_handles()?;
            }
            None
        };

        self.items = candidate_items;
        Ok(value)
    }

    pub fn run_script_text(&mut self, path: &str, raw: &str) -> Result<RuntimeValue, SessionError> {
        let parsed = parse_external_script(path, raw)?;
        let items = merge_items(&self.items, parsed.items);
        let source = self.synthetic_source(&items, None, parsed.result_source.as_deref());

        let frontend = analyze_source(&SourceText::new(path, self.next_revision(), &source))
            .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;
        self.validate_environment(&frontend.syntax)?;

        self.evaluate_script_source(&source)
    }

    pub fn drop_item(&mut self, raw: &str) -> Result<bool, SessionError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SessionError::Message("`:drop` requires a name".to_owned()));
        }

        let target = if trimmed == "$" {
            LAST_VALUE_NAME
        } else {
            trimmed
        };
        let before = self.items.len();
        self.items.retain(|item| !item.matches_drop(target));
        let removed_hidden = if target == LAST_VALUE_NAME {
            let had_hidden = self.hidden_last.is_some();
            self.clear_hidden_last()?;
            had_hidden
        } else {
            false
        };
        if before != self.items.len() {
            self.clear_binding_handles()?;
            self.retain_known_optimization_overrides();
        }
        Ok(before != self.items.len() || removed_hidden)
    }

    pub fn reset(&mut self) -> Result<(), SessionError> {
        self.clear_binding_handles()?;
        self.clear_hidden_last()?;
        self.items.clear();
        self.opt_overrides.clear();
        self.unload_active_artifact()?;
        Ok(())
    }

    pub fn snapshot_source(&self) -> String {
        self.synthetic_source(&self.items, self.hidden_last_item(), None)
    }

    pub fn restore_snapshot_source(&mut self, label: &str, text: &str) -> Result<(), SessionError> {
        let frontend = analyze_source(&SourceText::new(label, 1, text))
            .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;

        if !matches!(frontend.header.kind, ModuleKind::Script { .. }) {
            return Err(SessionError::Message(
                "snapshot must contain a script state".to_owned(),
            ));
        }

        self.validate_environment(&frontend.syntax)?;
        self.clear_binding_handles()?;
        self.clear_hidden_last()?;

        let restored = normalize_items(rebuild_items_from_unit(text, &frontend.syntax));
        let (hidden_last, items): (Vec<_>, Vec<_>) =
            restored.into_iter().partition(StoredItem::is_hidden_last);
        self.items = items;
        self.hidden_last = hidden_last
            .into_iter()
            .next()
            .map(|item| RetainedLastValue { item, value: None });
        self.retain_known_optimization_overrides();
        Ok(())
    }

    pub fn set_default_xopt(&mut self, xopt: OptimizationLevel) -> Result<(), SessionError> {
        self.default_xopt = xopt;
        self.unload_active_artifact()?;
        Ok(())
    }

    pub fn set_optimization(
        &mut self,
        xopt: OptimizationLevel,
        objects: &[String],
    ) -> Result<(), SessionError> {
        if objects.is_empty() {
            self.default_xopt = xopt;
        } else {
            let known_functions = self
                .items
                .iter()
                .filter_map(StoredItem::function_name)
                .collect::<BTreeSet<_>>();
            for object in objects {
                let object = normalize_optimization_object(object);
                if object == "module" {
                    self.default_xopt = xopt;
                } else if known_functions.contains(object.as_str()) {
                    if xopt == self.default_xopt {
                        self.opt_overrides.remove(&object);
                    } else {
                        self.opt_overrides.insert(object, xopt);
                    }
                } else {
                    return Err(SessionError::Message(format!(
                        "unknown optimization object `{object}`"
                    )));
                }
            }
        }

        self.recompile_active_artifact()
    }

    pub fn optimization_status(
        &mut self,
        object: Option<&str>,
    ) -> Result<Vec<OptimizationStatus>, SessionError> {
        let mut statuses = self.session_optimization_statuses()?;
        if let Some(object) = object {
            let object = normalize_optimization_object(object);
            statuses.retain(|status| status.object == object);
            if statuses.is_empty() {
                return Err(SessionError::Message(format!(
                    "unknown optimization object `{object}`"
                )));
            }
        }
        Ok(statuses)
    }

    pub fn optimization_dump(
        &mut self,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, SessionError> {
        let object = normalize_optimization_object(object);
        let Some(artifact_id) = self.active_artifact else {
            return Ok(None);
        };
        self.runner
            .optimization_dump(artifact_id, &object, kind)
            .map_err(SessionError::from)
    }

    fn validate_environment(
        &self,
        unit: &CompilationUnit,
    ) -> Result<TypeEnvironment, SessionError> {
        infer_environment(unit, &self.runner.package_manifests()?).map_err(compile_error)
    }

    fn analyze_items(
        &self,
        label: &str,
        items: &[StoredItem],
    ) -> Result<SessionModel, SessionError> {
        let source = render_session_source(items, None, None);
        let frontend = analyze_source(&SourceText::new(label, 1, &source))
            .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;
        let environment = self.validate_environment(&frontend.syntax)?;
        Ok(SessionModel {
            unit: frontend.syntax,
            environment,
        })
    }

    fn enforce_incremental_redefinition_policy(
        &self,
        incoming: &[StoredItem],
    ) -> Result<(), SessionError> {
        let changed_functions = incoming
            .iter()
            .filter_map(StoredItem::function_name)
            .filter(|name| {
                self.items
                    .iter()
                    .any(|item| item.function_name() == Some(*name))
            })
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if changed_functions.is_empty() {
            return Ok(());
        }

        let current = self.analyze_items("<repl-current>", &self.items)?;
        let current_functions = current
            .environment
            .functions
            .iter()
            .map(|function| (function.name.clone(), function))
            .collect::<BTreeMap<_, _>>();
        let direct_callers = collect_direct_callers(&current.unit);
        let affected_callers = changed_functions
            .iter()
            .flat_map(|name| direct_callers.get(name).into_iter().flatten().cloned())
            .collect::<BTreeSet<_>>();
        if affected_callers.is_empty() {
            return Ok(());
        }

        let filtered_existing = self
            .items
            .iter()
            .filter(|item| {
                item.function_name().is_none_or(|name| {
                    !changed_functions.contains(name) && !affected_callers.contains(name)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let candidate_items = merge_items(&filtered_existing, incoming.to_vec());
        let candidate = match self.analyze_items("<repl-redefine>", &candidate_items) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(()),
        };
        let candidate_functions = candidate
            .environment
            .functions
            .iter()
            .map(|function| (function.name.clone(), function))
            .collect::<BTreeMap<_, _>>();
        let incoming_function_names = incoming
            .iter()
            .filter_map(StoredItem::function_name)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();

        let mut violations = Vec::new();
        for function_name in changed_functions {
            let Some(old_summary) = current_functions.get(&function_name) else {
                continue;
            };
            let Some(new_summary) = candidate_functions.get(&function_name) else {
                continue;
            };
            if old_summary == new_summary {
                continue;
            }

            let missing_callers = direct_callers
                .get(&function_name)
                .into_iter()
                .flatten()
                .filter(|caller| !incoming_function_names.contains(caller.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if !missing_callers.is_empty() {
                violations.push((function_name, missing_callers));
            }
        }

        if violations.is_empty() {
            return Ok(());
        }

        let hint_names = redefinition_edit_hint(&violations);
        let lines = violations
            .into_iter()
            .map(|(function, callers)| {
                format!(
                    "changing signature of `{function}` requires redefining direct callers in the same chunk: {}",
                    callers
                        .iter()
                        .map(|caller| format!("`{caller}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>();
        Err(SessionError::Message(format!(
            "{}\nuse one submission, `:run`, or `:edit {}`",
            lines.join("\n"),
            hint_names
        )))
    }

    fn synthetic_source(
        &self,
        items: &[StoredItem],
        hidden_last: Option<&StoredItem>,
        result: Option<&str>,
    ) -> String {
        render_session_source(items, hidden_last, result)
    }

    fn evaluate_script_source(&mut self, source: &str) -> Result<RuntimeValue, SessionError> {
        let revision = self.next_revision();
        let compiled = SourceText::new("<repl-eval>", revision, source);
        let settings = self.optimization_settings();

        let artifact_id = if let Some(artifact_id) = self.active_artifact {
            self.runner
                .reload_script_with_settings(artifact_id, compiled, settings)?;
            artifact_id
        } else {
            let artifact_id = self.runner.load_script_with_settings(compiled, settings)?;
            self.active_artifact = Some(artifact_id);
            artifact_id
        };

        self.runner
            .run_script(artifact_id, &[])
            .map_err(SessionError::from)
    }

    fn unload_active_artifact(&mut self) -> Result<(), SessionError> {
        if let Some(artifact_id) = self.active_artifact.take() {
            self.runner.unload_script(artifact_id)?;
        }
        Ok(())
    }

    fn recompile_active_artifact(&mut self) -> Result<(), SessionError> {
        let Some(artifact_id) = self.active_artifact else {
            return Ok(());
        };
        let source = self.snapshot_source();
        let revision = self.next_revision();
        let compiled = SourceText::new("<repl-opt>", revision, source);
        self.runner.reload_script_with_settings(
            artifact_id,
            compiled,
            self.optimization_settings(),
        )?;
        Ok(())
    }

    fn session_optimization_statuses(&mut self) -> Result<Vec<OptimizationStatus>, SessionError> {
        if let Some(artifact_id) = self.active_artifact {
            return self
                .runner
                .optimization_status(artifact_id, &self.optimization_settings())
                .map_err(SessionError::from);
        }

        let functions = self
            .items
            .iter()
            .filter_map(StoredItem::function_name)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut statuses = vec![OptimizationStatus {
            object: "module".to_owned(),
            requested: self.default_xopt,
            rank: None,
            artifact: None,
            mir_available: false,
            wasm_available: false,
            runtime_note: None,
        }];
        statuses.extend(functions.into_iter().map(|function| {
            OptimizationStatus {
                requested: self
                    .opt_overrides
                    .get(&function)
                    .copied()
                    .unwrap_or(self.default_xopt),
                object: function,
                rank: None,
                artifact: None,
                mir_available: false,
                wasm_available: false,
                runtime_note: None,
            }
        }));
        Ok(statuses)
    }

    fn optimization_settings(&self) -> OptimizationSettings {
        OptimizationSettings {
            default: self.default_xopt,
            overrides: self.opt_overrides.clone(),
        }
    }

    fn retain_known_optimization_overrides(&mut self) {
        let known_functions = self
            .items
            .iter()
            .filter_map(StoredItem::function_name)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        self.opt_overrides
            .retain(|object, _| known_functions.contains(object));
    }

    fn next_revision(&mut self) -> u64 {
        self.next_source_revision += 1;
        self.next_source_revision
    }

    fn hidden_last_item(&self) -> Option<&StoredItem> {
        self.hidden_last.as_ref().map(|value| &value.item)
    }

    fn clear_binding_handles(&mut self) -> Result<(), SessionError> {
        let handles = self.binding_handles.values().copied().collect::<Vec<_>>();
        self.binding_handles.clear();
        for handle in handles {
            self.runner.release_handle(handle)?;
        }
        Ok(())
    }

    fn clear_hidden_last(&mut self) -> Result<(), SessionError> {
        let Some(hidden_last) = self.hidden_last.take() else {
            return Ok(());
        };
        if let Some(value) = hidden_last.value.as_ref() {
            self.release_runtime_value(value)?;
        }
        Ok(())
    }

    fn finalize_submission_result(
        &mut self,
        result_source: &str,
        identity_name: Option<&str>,
        value: RuntimeValue,
    ) -> Result<RuntimeValue, SessionError> {
        let mut value = value;
        let mut retain_for_hidden_last = false;

        if let Some(name) = identity_name {
            if name == LAST_VALUE_NAME {
                if let Some(existing) = self
                    .hidden_last
                    .as_ref()
                    .and_then(|hidden_last| hidden_last.value.clone())
                {
                    self.release_runtime_value(&value)?;
                    value = existing;
                    retain_for_hidden_last = true;
                }
            } else if let Some(&handle) = self.binding_handles.get(name) {
                self.release_runtime_value(&value)?;
                value = RuntimeValue::Handle(handle);
                retain_for_hidden_last = true;
            } else if let RuntimeValue::Handle(handle) = value {
                self.binding_handles.insert(name.to_owned(), handle);
                value = RuntimeValue::Handle(handle);
                retain_for_hidden_last = true;
            }
        }

        self.replace_hidden_last(result_source, value.clone(), retain_for_hidden_last)?;
        Ok(value)
    }

    fn replace_hidden_last(
        &mut self,
        result_source: &str,
        value: RuntimeValue,
        retain_value: bool,
    ) -> Result<(), SessionError> {
        if retain_value {
            self.retain_runtime_value(&value)?;
        }

        let previous = self.hidden_last.replace(RetainedLastValue {
            item: stored_last_value(result_source, &value),
            value: Some(value),
        });
        if let Some(previous) = previous {
            if let Some(value) = previous.value.as_ref() {
                self.release_runtime_value(value)?;
            }
        }
        Ok(())
    }

    fn retain_runtime_value(&self, value: &RuntimeValue) -> Result<(), SessionError> {
        if let RuntimeValue::Handle(handle) = value {
            self.runner.retain_handle(*handle)?;
        }
        Ok(())
    }

    fn release_runtime_value(&self, value: &RuntimeValue) -> Result<(), SessionError> {
        if let RuntimeValue::Handle(handle) = value {
            self.runner.release_handle(*handle)?;
        }
        Ok(())
    }
}

impl<R: RuntimeRunner> Drop for SessionState<R> {
    fn drop(&mut self) {
        let _ = self.clear_binding_handles();
        let _ = self.clear_hidden_last();
        let _ = self.unload_active_artifact();
    }
}

#[derive(Debug)]
pub struct InteractiveSession<R: RuntimeRunner> {
    runner: R,
    session_id: SessionId,
}

impl<R: RuntimeRunner> InteractiveSession<R> {
    pub fn new(runner: R) -> Result<Self, SessionError> {
        Self::open(
            runner,
            SessionOpenRequest {
                selector: None,
                mode: SessionOpenMode::Create,
            },
        )
    }

    pub fn named(runner: R, name: impl AsRef<str>) -> Result<Self, SessionError> {
        Self::open(
            runner,
            SessionOpenRequest {
                selector: Some(SessionSelector::Name(name.as_ref().to_owned())),
                mode: SessionOpenMode::AttachOrCreate,
            },
        )
    }

    pub fn attach(runner: R, selector: SessionSelector) -> Result<Self, SessionError> {
        Self::open(
            runner,
            SessionOpenRequest {
                selector: Some(selector),
                mode: SessionOpenMode::Attach,
            },
        )
    }

    pub fn create_named(runner: R, name: impl AsRef<str>) -> Result<Self, SessionError> {
        Self::open(
            runner,
            SessionOpenRequest {
                selector: Some(SessionSelector::Name(name.as_ref().to_owned())),
                mode: SessionOpenMode::Create,
            },
        )
    }

    pub fn open(runner: R, request: SessionOpenRequest) -> Result<Self, SessionError> {
        let session_id = runner.open_session(request).map_err(SessionError::from)?;
        Ok(Self { runner, session_id })
    }

    pub fn id(&self) -> SessionId {
        self.session_id
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        self.runner.list_sessions().map_err(SessionError::from)
    }

    pub fn set_reserved(&self, reserved: bool) -> Result<(), SessionError> {
        self.runner
            .set_session_reserved(self.session_id, reserved)
            .map_err(SessionError::from)
    }

    pub fn completion(&self) -> Result<SessionCompletion, SessionError> {
        completion_from_snapshot(&self.runner, &self.snapshot_source()?)
    }

    pub fn eval(&mut self, raw: &str) -> Result<Option<RuntimeValue>, SessionError> {
        self.runner
            .evaluate_session_submission(self.session_id, raw)
            .map_err(SessionError::from)
    }

    pub fn evaluate_submission(&mut self, raw: &str) -> Result<Option<RuntimeValue>, SessionError> {
        self.eval(raw)
    }

    pub fn run_script_text(&mut self, path: &str, raw: &str) -> Result<RuntimeValue, SessionError> {
        self.runner
            .run_session_script_text(self.session_id, path, raw)
            .map_err(SessionError::from)
    }

    pub fn type_of(&self, raw_expr: &str) -> Result<ReplType, SessionError> {
        if raw_expr.trim().is_empty() {
            return Err(SessionError::Message(
                "`:type` requires an expression".to_owned(),
            ));
        }

        let rewritten = rewrite_last_shorthand(raw_expr);
        let snapshot = self.snapshot_source()?;
        let items = snapshot_items(&snapshot, true)?;
        let source = render_session_source(
            &items,
            rewritten
                .uses_last_value
                .then(|| find_hidden_last(&items))
                .flatten(),
            Some(&rewritten.source),
        );
        let frontend = analyze_source(&SourceText::new("<repl-type>", 1, &source))
            .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;
        let environment = infer_environment(&frontend.syntax, &self.runner.package_manifests()?)
            .map_err(compile_error)?;
        Ok(environment.result.unwrap_or(ReplType::Unit))
    }

    pub fn drop_item(&mut self, raw: &str) -> Result<bool, SessionError> {
        self.runner
            .drop_session_item(self.session_id, raw)
            .map_err(SessionError::from)
    }

    pub fn reset(&mut self) -> Result<(), SessionError> {
        self.runner
            .reset_session(self.session_id)
            .map_err(SessionError::from)
    }

    pub fn snapshot_source(&self) -> Result<String, SessionError> {
        self.runner
            .snapshot_session_source(self.session_id)
            .map_err(SessionError::from)
    }

    pub fn restore_snapshot_source(&mut self, label: &str, text: &str) -> Result<(), SessionError> {
        self.runner
            .restore_session_snapshot(self.session_id, label, text)
            .map_err(SessionError::from)
    }

    pub fn current_environment(
        &self,
        include_hidden_last: bool,
    ) -> Result<TypeEnvironment, SessionError> {
        environment_from_snapshot(&self.runner, &self.snapshot_source()?, include_hidden_last)
    }

    pub fn set_default_xopt(&mut self, xopt: OptimizationLevel) -> Result<(), SessionError> {
        self.runner
            .set_session_default_xopt(self.session_id, xopt)
            .map_err(SessionError::from)
    }

    pub fn set_optimization(
        &mut self,
        xopt: OptimizationLevel,
        objects: &[String],
    ) -> Result<(), SessionError> {
        self.runner
            .set_session_optimization(self.session_id, xopt, objects)
            .map_err(SessionError::from)
    }

    pub fn optimization_status(
        &self,
        object: Option<&str>,
    ) -> Result<Vec<OptimizationStatus>, SessionError> {
        self.runner
            .session_optimization_status(self.session_id, object)
            .map_err(SessionError::from)
    }

    pub fn optimization_dump(
        &self,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, SessionError> {
        self.runner
            .session_optimization_dump(self.session_id, object, kind)
            .map_err(SessionError::from)
    }

    pub fn live_handles(&self) -> Result<Vec<HandleId>, SessionError> {
        Ok(self.runner.live_handles()?)
    }

    pub fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, SessionError> {
        Ok(self.runner.describe_handle(handle)?)
    }

    pub fn read_handle_data(
        &self,
        handle: HandleId,
        offset: u64,
        max_bytes: u32,
    ) -> Result<HandleDataChunk, SessionError> {
        Ok(self.runner.read_handle_data(handle, offset, max_bytes)?)
    }

    pub fn get_handle_data(&self, handle: HandleId) -> Result<HandleData, SessionError> {
        Ok(self.runner.get_handle_data(handle)?)
    }

    pub fn package_manifests(&self) -> Result<Vec<PackageManifest>, SessionError> {
        Ok(self.runner.package_manifests()?)
    }
}

impl<R: RuntimeRunner> Drop for InteractiveSession<R> {
    fn drop(&mut self) {
        let _ = self.runner.close_session(self.session_id);
    }
}

fn normalize_optimization_object(object: &str) -> String {
    let trimmed = object.trim();
    if trimmed.is_empty() || matches!(trimmed, "module" | ".") {
        "module".to_owned()
    } else if trimmed == "$" {
        LAST_VALUE_NAME.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn completion_from_snapshot<R: RuntimeRunner>(
    runner: &R,
    snapshot: &str,
) -> Result<SessionCompletion, SessionError> {
    let mut completion = SessionCompletion {
        handles: runner
            .live_handles()?
            .into_iter()
            .map(|handle| handle.0.to_string())
            .collect(),
        symbols: language_keywords(),
    };

    completion.symbols.push("$".to_owned());
    let items = snapshot_items(snapshot, true)?;
    for item in &items {
        if !item.is_hidden_last() {
            completion.symbols.push(item.display_name().to_owned());
        }
    }

    for manifest in runner.package_manifests()? {
        extend_manifest_symbols(&mut completion.symbols, &manifest);
    }

    if let Ok(environment) = environment_from_snapshot(runner, snapshot, true) {
        completion.symbols.extend(environment.imports);
        completion.symbols.extend(
            environment
                .bindings
                .into_iter()
                .filter(|binding| binding.name != LAST_VALUE_NAME)
                .map(|binding| binding.name),
        );
        completion.symbols.extend(
            environment
                .functions
                .into_iter()
                .map(|function| function.name),
        );
    }

    completion.symbols.sort();
    completion.symbols.dedup();
    Ok(completion)
}

fn environment_from_snapshot<R: RuntimeRunner>(
    runner: &R,
    snapshot: &str,
    include_hidden_last: bool,
) -> Result<TypeEnvironment, SessionError> {
    let items = snapshot_items(snapshot, include_hidden_last)?;
    let source = render_session_source(
        &items,
        if include_hidden_last {
            find_hidden_last(&items)
        } else {
            None
        },
        None,
    );
    let frontend = analyze_source(&SourceText::new("<repl-env>", 1, &source))
        .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;
    infer_environment(&frontend.syntax, &runner.package_manifests()?).map_err(compile_error)
}

fn snapshot_items(
    snapshot: &str,
    include_hidden_last: bool,
) -> Result<Vec<StoredItem>, SessionError> {
    let frontend = analyze_source(&SourceText::new("<repl-snapshot>", 1, snapshot))
        .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;
    let mut items = normalize_items(rebuild_items_from_unit(snapshot, &frontend.syntax));
    if !include_hidden_last {
        items.retain(|item| !item.is_hidden_last());
    }
    Ok(items)
}

fn find_hidden_last(items: &[StoredItem]) -> Option<&StoredItem> {
    items.iter().find(|item| item.is_hidden_last())
}

fn render_session_source(
    items: &[StoredItem],
    hidden_last: Option<&StoredItem>,
    result: Option<&str>,
) -> String {
    let mut source = format!("script {REPL_MODULE};\n");
    for item in items {
        if item.is_hidden_last() {
            continue;
        }
        source.push_str(&item.source);
        if !item.source.ends_with('\n') {
            source.push('\n');
        }
    }
    if let Some(item) = hidden_last {
        source.push_str(&item.source);
        if !item.source.ends_with('\n') {
            source.push('\n');
        }
    }
    if let Some(result) = result {
        source.push_str(result);
        source.push('\n');
    }
    source
}

fn parse_submission(raw: &str) -> Result<ParsedSubmission, SessionError> {
    parse_script_fragment("<repl-submit>", raw)
}

fn parse_external_script(path: &str, raw: &str) -> Result<ParsedSubmission, SessionError> {
    let frontend = analyze_source(&SourceText::new(path, 1, raw))
        .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;
    if !matches!(frontend.header.kind, ModuleKind::Script { .. }) {
        return Err(SessionError::Message(
            "`:run` requires a script file".to_owned(),
        ));
    }
    Ok(ParsedSubmission {
        items: rebuild_items_from_unit(raw, &frontend.syntax),
        result_source: frontend
            .syntax
            .result
            .as_ref()
            .map(|expr| slice_source(raw, expr)),
        identity_name: frontend
            .syntax
            .result
            .as_ref()
            .and_then(result_identity_name),
        uses_last_value: false,
    })
}

fn parse_script_fragment(path: &str, raw: &str) -> Result<ParsedSubmission, SessionError> {
    let rewritten = rewrite_last_shorthand(raw);
    let wrapped = format!("script {REPL_MODULE};\n{}\n", rewritten.source);
    let frontend = analyze_source(&SourceText::new(path, 1, &wrapped))
        .map_err(|diagnostics| compile_error(diagnostics.to_string()))?;
    Ok(ParsedSubmission {
        items: rebuild_items_from_unit(&wrapped, &frontend.syntax),
        result_source: frontend
            .syntax
            .result
            .as_ref()
            .map(|expr| slice_source(&wrapped, expr)),
        identity_name: frontend
            .syntax
            .result
            .as_ref()
            .and_then(result_identity_name),
        uses_last_value: rewritten.uses_last_value,
    })
}

fn result_identity_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) if name.segments.len() == 1 => Some(name.segments[0].clone()),
        _ => None,
    }
}

fn rebuild_items_from_unit(source: &str, unit: &CompilationUnit) -> Vec<StoredItem> {
    unit.items
        .iter()
        .map(|item| StoredItem {
            key: item_key(item),
            source: slice_item_source(source, item),
        })
        .collect()
}

fn normalize_items(items: Vec<StoredItem>) -> Vec<StoredItem> {
    merge_items(&[], items)
}

fn merge_items(existing: &[StoredItem], incoming: Vec<StoredItem>) -> Vec<StoredItem> {
    let mut merged = existing.to_vec();
    for item in incoming {
        if item.key.replaces_existing() {
            merged.retain(|current| !current.key.conflicts_with(&item.key));
        }
        merged.push(item);
    }
    merged
}

fn collect_direct_callers(unit: &CompilationUnit) -> BTreeMap<String, BTreeSet<String>> {
    let module_segments = REPL_MODULE
        .split('.')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let local_functions = unit
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Function(function) => Some(function.name.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut direct_callers = BTreeMap::<String, BTreeSet<String>>::new();

    for item in &unit.items {
        let TopLevelItem::Function(function) = item else {
            continue;
        };
        let callees = FunctionCallCollector::new(&local_functions, &module_segments)
            .collect_from_function(function);
        for callee in callees {
            direct_callers
                .entry(callee)
                .or_default()
                .insert(function.name.clone());
        }
    }

    direct_callers
}

fn redefinition_edit_hint(violations: &[(String, Vec<String>)]) -> String {
    let mut names = BTreeSet::new();
    for (function, callers) in violations {
        names.insert(function.clone());
        names.extend(callers.iter().cloned());
    }
    names.into_iter().collect::<Vec<_>>().join(" ")
}

struct FunctionCallCollector<'a> {
    local_functions: &'a BTreeSet<String>,
    module_segments: &'a [String],
    scopes: Vec<BTreeSet<String>>,
    calls: BTreeSet<String>,
}

impl<'a> FunctionCallCollector<'a> {
    fn new(local_functions: &'a BTreeSet<String>, module_segments: &'a [String]) -> Self {
        Self {
            local_functions,
            module_segments,
            scopes: Vec::new(),
            calls: BTreeSet::new(),
        }
    }

    fn collect_from_function(mut self, function: &FunctionDecl) -> BTreeSet<String> {
        self.push_scope();
        for parameter in &function.parameters {
            self.bind_name(&parameter.name);
        }
        for parameter in &function.parameters {
            if let Some(default) = &parameter.default {
                self.visit_expr(default);
            }
        }
        self.visit_expr(&function.body);
        self.pop_scope();
        self.calls
    }

    fn push_scope(&mut self) {
        self.scopes.push(BTreeSet::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind_name(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned());
        }
    }

    fn is_shadowed(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }

    fn resolve_local_function(&self, name: &QualifiedName) -> Option<String> {
        match name.segments.as_slice() {
            [local] if !self.is_shadowed(local) && self.local_functions.contains(local) => {
                Some(local.clone())
            }
            segments
                if segments.len() == self.module_segments.len() + 1
                    && segments[..self.module_segments.len()]
                        .iter()
                        .zip(self.module_segments.iter())
                        .all(|(left, right)| left == right) =>
            {
                let local = segments.last()?.clone();
                self.local_functions.contains(&local).then_some(local)
            }
            _ => None,
        }
    }

    fn record_call(&mut self, name: &QualifiedName) {
        if let Some(local) = self.resolve_local_function(name) {
            self.calls.insert(local);
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Name(_) => {}
            ExprKind::String(literal) => self.visit_string_literal(literal),
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    self.visit_expr(item);
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    self.visit_expr(&field.value);
                }
            }
            ExprKind::Call { callee, arguments } => {
                if let ExprKind::Name(name) = &callee.kind {
                    self.record_call(name);
                }
                self.visit_expr(callee);
                for argument in arguments {
                    self.visit_argument(argument);
                }
            }
            ExprKind::Intrinsic(intrinsic) => self.visit_intrinsic(intrinsic),
            ExprKind::Index { target, index } => {
                self.visit_expr(target);
                self.visit_expr(index);
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => self.visit_expr(target),
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => {
                self.record_call(callee);
                self.visit_expr(receiver);
                for argument in arguments {
                    self.visit_argument(argument);
                }
            }
            ExprKind::Unary { expr, .. } => self.visit_expr(expr),
            ExprKind::Binary { left, right, .. } => {
                self.visit_expr(left);
                self.visit_expr(right);
            }
            ExprKind::Range(range) => self.visit_range(range),
            ExprKind::If(expr) => self.visit_if(expr),
            ExprKind::When(expr) => self.visit_when(expr),
            ExprKind::For(expr) => self.visit_for(expr),
            ExprKind::Lambda(expr) => self.visit_lambda(expr),
            ExprKind::Block(block) => self.visit_block(block),
        }
    }

    fn visit_argument(&mut self, argument: &Argument) {
        match argument {
            Argument::Positional(expr) => self.visit_expr(expr),
            Argument::Named { value, .. } => self.visit_expr(value),
        }
    }

    fn visit_intrinsic(&mut self, intrinsic: &IntrinsicExpr) {
        match intrinsic {
            IntrinsicExpr::Updated(updated) => self.visit_updated(updated),
            IntrinsicExpr::Econ(EconIntrinsic { body, .. }) => self.visit_block(body),
        }
    }

    fn visit_updated(&mut self, updated: &UpdatedIntrinsic) {
        self.visit_expr(&updated.target);
        for update in &updated.updates {
            self.visit_updated_arg(update);
        }
    }

    fn visit_updated_arg(&mut self, update: &UpdatedArg) {
        self.visit_expr(&update.value);
    }

    fn visit_range(&mut self, range: &RangeExpr) {
        if let Some(start) = &range.start {
            self.visit_expr(start);
        }
        if let Some(end) = &range.end {
            self.visit_expr(end);
        }
    }

    fn visit_if(&mut self, expr: &IfExpr) {
        for branch in &expr.branches {
            self.visit_expr(&branch.condition);
            self.visit_block(&branch.body);
        }
        if let Some(else_branch) = &expr.else_branch {
            self.visit_block(else_branch);
        }
    }

    fn visit_when(&mut self, expr: &WhenExpr) {
        self.visit_expr(&expr.subject);
        for arm in &expr.arms {
            self.visit_when_arm(arm);
        }
        if let Some(else_arm) = &expr.else_arm {
            self.visit_expr(else_arm);
        }
    }

    fn visit_when_arm(&mut self, arm: &WhenArm) {
        self.push_scope();
        if let Some(binding) = &arm.binding {
            self.bind_name(binding);
        }
        self.visit_expr(&arm.body);
        self.pop_scope();
    }

    fn visit_lambda(&mut self, expr: &LambdaExpr) {
        self.push_scope();
        for parameter in &expr.parameters {
            self.bind_name(&parameter.name);
        }
        self.visit_expr(&expr.body);
        self.pop_scope();
    }

    fn visit_block(&mut self, block: &BlockExpr) {
        self.push_scope();
        self.visit_block_contents(block);
        self.pop_scope();
    }

    fn visit_block_contents(&mut self, block: &BlockExpr) {
        for item in &block.items {
            match item {
                BlockItem::LocalValue(value) => self.visit_local_value(value),
                BlockItem::Assignment(assignment) => self.visit_assignment(assignment),
                BlockItem::CompoundAssignment(assignment) => {
                    self.visit_compound_assignment(assignment)
                }
                BlockItem::Return(statement) => self.visit_return(statement),
                BlockItem::Panic(statement) => self.visit_string_literal(&statement.message),
                BlockItem::BlockStatement(expr) | BlockItem::Expr(expr) => self.visit_expr(expr),
            }
        }
        if let Some(trailing) = &block.trailing {
            self.visit_expr(trailing);
        }
    }

    fn visit_local_value(&mut self, value: &LocalValueDecl) {
        self.visit_expr(&value.initializer);
        self.bind_name(&value.name);
    }

    fn visit_assignment(&mut self, assignment: &AssignmentStatement) {
        self.visit_expr(&assignment.value);
    }

    fn visit_compound_assignment(&mut self, assignment: &CompoundAssignmentStatement) {
        self.visit_expr(&assignment.value);
    }

    fn visit_for(&mut self, statement: &ForExpr) {
        self.visit_expr(&statement.iterable);
        self.push_scope();
        self.bind_name(&statement.pattern);
        self.visit_block_contents(&statement.body);
        self.pop_scope();
    }

    fn visit_return(&mut self, statement: &ReturnStatement) {
        if let Some(value) = &statement.value {
            self.visit_expr(value);
        }
    }

    fn visit_string_literal(&mut self, literal: &StringLiteral) {
        for part in &literal.parts {
            if let StringPart::Interpolation(expr) = part {
                self.visit_expr(expr);
            }
        }
    }
}

fn item_key(item: &TopLevelItem) -> StoredItemKey {
    match item {
        TopLevelItem::Import(import) => StoredItemKey::Import {
            module: import.module.to_source_string(),
        },
        TopLevelItem::Value(value) => StoredItemKey::Value {
            name: value.name.clone(),
        },
        TopLevelItem::Function(function) => StoredItemKey::Function {
            name: function.name.clone(),
        },
        TopLevelItem::Param(param) => StoredItemKey::Value {
            name: param.name.clone(),
        },
        TopLevelItem::Statement(_) => StoredItemKey::Statement,
    }
}

fn slice_item_source(source: &str, item: &TopLevelItem) -> String {
    let span = match item {
        TopLevelItem::Import(import) => &import.span,
        TopLevelItem::Param(param) => &param.span,
        TopLevelItem::Value(value) => &value.span,
        TopLevelItem::Function(function) => &function.span,
        TopLevelItem::Statement(statement) => match statement {
            BlockItem::LocalValue(value) => &value.span,
            BlockItem::Assignment(assignment) => &assignment.span,
            BlockItem::CompoundAssignment(assignment) => &assignment.span,
            BlockItem::Return(statement) => &statement.span,
            BlockItem::Panic(statement) => &statement.span,
            BlockItem::BlockStatement(expr) | BlockItem::Expr(expr) => &expr.span,
        },
    };
    source[span.start..span.end].to_owned()
}

fn slice_source(source: &str, expr: &Expr) -> String {
    source[expr.span.start..expr.span.end].to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RewrittenInput {
    source: String,
    uses_last_value: bool,
}

fn rewrite_last_shorthand(raw: &str) -> RewrittenInput {
    let mut out = String::new();
    let chars = raw.chars();
    let mut in_string = false;
    let mut escape = false;
    let mut uses_last_value = false;

    for ch in chars {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }

        if ch == '$' {
            out.push_str(LAST_VALUE_NAME);
            uses_last_value = true;
            continue;
        }

        out.push(ch);
    }

    RewrittenInput {
        source: out,
        uses_last_value,
    }
}

fn stored_last_value(result_source: &str, value: &RuntimeValue) -> StoredItem {
    let source = match value {
        RuntimeValue::Inline(value) => {
            format!(
                "val {LAST_VALUE_NAME} = {};",
                render_inline_value_source(value)
            )
        }
        RuntimeValue::Handle(_) => format!("val {LAST_VALUE_NAME} = {result_source};"),
    };

    StoredItem {
        key: StoredItemKey::Value {
            name: LAST_VALUE_NAME.to_owned(),
        },
        source,
    }
}

fn render_inline_value_source(value: &vox_core::value::InlineValue) -> String {
    match value {
        vox_core::value::InlineValue::Int(value) => value.to_string(),
        vox_core::value::InlineValue::Float(value) => render_float_literal(*value),
        vox_core::value::InlineValue::Bool(value) => value.to_string(),
        vox_core::value::InlineValue::String(value) => {
            format!("\"{}\"", escape_string_literal(value))
        }
        vox_core::value::InlineValue::Handle(handle) => format!("<handle {}>", handle.0),
        vox_core::value::InlineValue::Tuple(values) => match values.as_slice() {
            [] => "()".to_owned(),
            [single] => format!("({},)", render_inline_value_source(single)),
            _ => format!(
                "({})",
                values
                    .iter()
                    .map(render_inline_value_source)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        vox_core::value::InlineValue::Record(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", render_inline_value_source(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        vox_core::value::InlineValue::Null => "null".to_owned(),
    }
}

fn render_float_literal(value: f64) -> String {
    let mut rendered = value.to_string();
    if value.is_finite() && !rendered.contains(['.', 'e', 'E']) {
        rendered.push_str(".0");
    }
    rendered
}

fn escape_string_literal(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '$' => escaped.push_str("\\$"),
            other => escaped.push(other),
        }
    }
    escaped
}
