use std::collections::BTreeMap;

use thiserror::Error;
use vox_compiler::front_end::{
    analyze_source,
    ast::{CompilationUnit, Expr, ExprKind, TopLevelItem},
};
use vox_core::{
    host::PackageManifest,
    ids::{ArtifactId, HandleId, SessionId},
    opt::OptimizationLevel,
    source::{ModuleKind, SourceText},
    value::{HandleSummary, RuntimeValue},
};

use crate::{
    ReplType, RunnerError, RuntimeRunner, SessionOpenMode, SessionOpenRequest, SessionSelector,
    SessionSummary, TypeEnvironment, extend_manifest_symbols, infer_environment, language_keywords,
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
}

impl StoredItem {
    fn display_name(&self) -> &str {
        match &self.key {
            StoredItemKey::Import { module } => module,
            StoredItemKey::Value { name } | StoredItemKey::Function { name } => name,
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
        }
    }

    fn is_hidden_last(&self) -> bool {
        matches!(&self.key, StoredItemKey::Value { name } if name == LAST_VALUE_NAME)
    }
}

impl StoredItemKey {
    fn conflicts_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Import { module: left }, Self::Import { module: right }) => left == right,
            (Self::Value { name: left }, Self::Value { name: right })
            | (Self::Value { name: left }, Self::Function { name: right })
            | (Self::Function { name: left }, Self::Value { name: right })
            | (Self::Function { name: left }, Self::Function { name: right }) => left == right,
            _ => false,
        }
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

#[derive(Debug, Clone)]
pub(crate) struct SessionState<R: RuntimeRunner> {
    runner: R,
    items: Vec<StoredItem>,
    binding_handles: BTreeMap<String, HandleId>,
    hidden_last: Option<RetainedLastValue>,
    next_source_revision: u64,
    active_artifact: Option<ArtifactId>,
    default_xopt: OptimizationLevel,
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
        }
    }

    pub fn evaluate_submission(&mut self, raw: &str) -> Result<Option<RuntimeValue>, SessionError> {
        let parsed = parse_submission(raw)?;
        if parsed.items.is_empty() && parsed.result_source.is_none() {
            return Ok(None);
        }

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

        let front_end = analyze_source(&SourceText::new(
            "<repl-submit>",
            self.next_revision(),
            &source,
        ))
        .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;

        self.validate_environment(&front_end.syntax)?;

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

        let front_end = analyze_source(&SourceText::new(path, self.next_revision(), &source))
            .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;
        self.validate_environment(&front_end.syntax)?;

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
        }
        Ok(before != self.items.len() || removed_hidden)
    }

    pub fn reset(&mut self) -> Result<(), SessionError> {
        self.clear_binding_handles()?;
        self.clear_hidden_last()?;
        self.items.clear();
        self.unload_active_artifact()?;
        Ok(())
    }

    pub fn snapshot_source(&self) -> String {
        self.synthetic_source(&self.items, self.hidden_last_item(), None)
    }

    pub fn restore_snapshot_source(&mut self, label: &str, text: &str) -> Result<(), SessionError> {
        let front_end = analyze_source(&SourceText::new(label, 1, text))
            .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;

        if !matches!(front_end.header.kind, ModuleKind::Script { .. }) {
            return Err(SessionError::Message(
                "snapshot must contain a script state".to_owned(),
            ));
        }

        self.validate_environment(&front_end.syntax)?;
        self.clear_binding_handles()?;
        self.clear_hidden_last()?;

        let restored = normalize_items(rebuild_items_from_unit(text, &front_end.syntax));
        let (hidden_last, items): (Vec<_>, Vec<_>) =
            restored.into_iter().partition(StoredItem::is_hidden_last);
        self.items = items;
        self.hidden_last = hidden_last
            .into_iter()
            .next()
            .map(|item| RetainedLastValue { item, value: None });
        Ok(())
    }

    pub fn set_default_xopt(&mut self, xopt: OptimizationLevel) -> Result<(), SessionError> {
        self.default_xopt = xopt;
        self.unload_active_artifact()?;
        Ok(())
    }

    fn validate_environment(
        &self,
        unit: &CompilationUnit,
    ) -> Result<TypeEnvironment, SessionError> {
        infer_environment(unit, &self.runner.package_manifests()?).map_err(SessionError::Message)
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

        let artifact_id = if let Some(artifact_id) = self.active_artifact {
            self.runner.reload_script(artifact_id, compiled)?;
            artifact_id
        } else {
            let artifact_id = self.runner.load_script(compiled, Some(self.default_xopt))?;
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

    pub fn evaluate_submission(&mut self, raw: &str) -> Result<Option<RuntimeValue>, SessionError> {
        self.runner
            .evaluate_session_submission(self.session_id, raw)
            .map_err(SessionError::from)
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
        let front_end = analyze_source(&SourceText::new("<repl-type>", 1, &source))
            .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;
        let environment = infer_environment(&front_end.syntax, &self.runner.package_manifests()?)
            .map_err(SessionError::Message)?;
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

    pub fn live_handles(&self) -> Result<Vec<HandleId>, SessionError> {
        Ok(self.runner.live_handles()?)
    }

    pub fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, SessionError> {
        Ok(self.runner.describe_handle(handle)?)
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
    let front_end = analyze_source(&SourceText::new("<repl-env>", 1, &source))
        .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;
    infer_environment(&front_end.syntax, &runner.package_manifests()?)
        .map_err(SessionError::Message)
}

fn snapshot_items(
    snapshot: &str,
    include_hidden_last: bool,
) -> Result<Vec<StoredItem>, SessionError> {
    let front_end = analyze_source(&SourceText::new("<repl-snapshot>", 1, snapshot))
        .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;
    let mut items = normalize_items(rebuild_items_from_unit(snapshot, &front_end.syntax));
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
    let front_end = analyze_source(&SourceText::new(path, 1, raw))
        .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;
    if !matches!(front_end.header.kind, ModuleKind::Script { .. }) {
        return Err(SessionError::Message(
            "`:run` requires a script file".to_owned(),
        ));
    }
    Ok(ParsedSubmission {
        items: rebuild_items_from_unit(raw, &front_end.syntax),
        result_source: front_end
            .syntax
            .result
            .as_ref()
            .map(|expr| slice_source(raw, expr)),
        identity_name: front_end
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
    let front_end = analyze_source(&SourceText::new(path, 1, &wrapped))
        .map_err(|diagnostics| SessionError::Message(diagnostics.to_string()))?;
    Ok(ParsedSubmission {
        items: rebuild_items_from_unit(&wrapped, &front_end.syntax),
        result_source: front_end
            .syntax
            .result
            .as_ref()
            .map(|expr| slice_source(&wrapped, expr)),
        identity_name: front_end
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
        merged.retain(|current| !current.key.conflicts_with(&item.key));
        merged.push(item);
    }
    merged
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
    }
}

fn slice_item_source(source: &str, item: &TopLevelItem) -> String {
    let span = match item {
        TopLevelItem::Import(import) => &import.span,
        TopLevelItem::Param(param) => &param.span,
        TopLevelItem::Value(value) => &value.span,
        TopLevelItem::Function(function) => &function.span,
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
