use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use vox_compiler::front_end::{
    analyze_source,
    ast::{CompilationUnit, Expr, TopLevelItem},
};
use vox_core::{
    host::PackageManifest,
    ids::HandleId,
    opt::OptimizationLevel,
    source::{ModuleKind, SourceText},
    value::RuntimeValue,
};
use vox_runtime::Runtime;

use crate::{
    CompletionSnapshot,
    command::ReplCommand,
    typing::{TypeEnvironment, infer_environment},
};

const REPL_MODULE: &str = "repl.session";
const LAST_VALUE_NAME: &str = "__repl_last";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplOutput {
    Message(String),
    Error(String),
    Exit,
}

impl ReplOutput {
    fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    fn error(message: impl Into<String>) -> Self {
        Self::Error(normalize_error_message(message.into()))
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
    uses_last_value: bool,
}

#[derive(Debug, Default)]
pub struct ReplSession {
    runtime: Runtime,
    items: Vec<StoredItem>,
    hidden_last: Option<StoredItem>,
    next_source_revision: u64,
}

impl ReplSession {
    pub fn handle_line(&mut self, line: &str) -> ReplOutput {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return ReplOutput::message(String::new());
        }

        if trimmed.starts_with(':') {
            return self.handle_command(trimmed);
        }

        self.evaluate_submission(line)
    }

    pub fn completion_snapshot(&self) -> CompletionSnapshot {
        let mut snapshot = CompletionSnapshot {
            commands: vec![
                ":help".to_owned(),
                ":quit".to_owned(),
                ":reset".to_owned(),
                ":clear".to_owned(),
                ":env".to_owned(),
                ":snapshot".to_owned(),
                ":restore".to_owned(),
                ":run".to_owned(),
                ":show".to_owned(),
                ":type".to_owned(),
                ":handles".to_owned(),
                ":drop".to_owned(),
                ":xopt".to_owned(),
            ],
            snapshots: available_snapshot_names(),
            xopts: vec!["NOpt".to_owned(), "IOpt".to_owned(), "SOpt".to_owned()],
            handles: self
                .runtime
                .live_handles()
                .into_iter()
                .map(|handle| handle.0.to_string())
                .collect(),
            symbols: language_keywords(),
        };

        snapshot.symbols.push("$".to_owned());
        for item in &self.items {
            snapshot.symbols.push(item.display_name().to_owned());
        }

        for manifest in self.runtime.package_manifests() {
            extend_manifest_symbols(&mut snapshot.symbols, &manifest);
        }

        if let Ok(environment) = self.current_environment(true) {
            snapshot.symbols.extend(environment.imports);
            snapshot.symbols.extend(
                environment
                    .bindings
                    .into_iter()
                    .filter(|binding| binding.name != LAST_VALUE_NAME)
                    .map(|binding| binding.name),
            );
            snapshot.symbols.extend(
                environment
                    .functions
                    .into_iter()
                    .map(|function| function.name),
            );
        }

        snapshot.symbols.sort();
        snapshot.symbols.dedup();
        snapshot.snapshots.sort();
        snapshot.snapshots.dedup();
        snapshot
    }

    fn handle_command(&mut self, line: &str) -> ReplOutput {
        match ReplCommand::from_str(line) {
            Ok(command) => self.execute(command),
            Err(message) => ReplOutput::error(message),
        }
    }

    fn execute(&mut self, command: ReplCommand) -> ReplOutput {
        match command {
            ReplCommand::Help => ReplOutput::message(render_help()),
            ReplCommand::Quit => ReplOutput::Exit,
            ReplCommand::Reset => {
                self.items.clear();
                self.hidden_last = None;
                self.runtime.clear_artifacts();
                ReplOutput::message("interactive state cleared".to_owned())
            }
            ReplCommand::Clear => ReplOutput::message("\x1b[2J\x1b[H".to_owned()),
            ReplCommand::Env => match self.current_environment(true) {
                Ok(environment) => ReplOutput::message(render_environment(&environment)),
                Err(message) => ReplOutput::error(message),
            },
            ReplCommand::Snapshot(name) => self.snapshot(&name),
            ReplCommand::Restore(name) => self.restore(&name),
            ReplCommand::TypeOf(expr) => self.type_of(&expr),
            ReplCommand::Run(path) => self.run_file(&path),
            ReplCommand::Handles => self.list_handles(),
            ReplCommand::Show(handle) => self.show_handle(&handle),
            ReplCommand::Drop(name) => self.drop(&name),
            ReplCommand::XOpt(mode) => {
                let xopt = match mode.as_str() {
                    "NOpt" => OptimizationLevel::NOpt,
                    "IOpt" => OptimizationLevel::IOpt,
                    "SOpt" => OptimizationLevel::SOpt,
                    _ => {
                        return ReplOutput::error(format!("unknown optimization mode `{mode}`"));
                    }
                };
                self.runtime.set_default_xopt(xopt);
                ReplOutput::message(format!("default optimization mode set to {mode}"))
            }
        }
    }

    fn evaluate_submission(&mut self, raw: &str) -> ReplOutput {
        let parsed = match parse_submission(raw) {
            Ok(parsed) => parsed,
            Err(message) => return ReplOutput::error(message),
        };

        if parsed.items.is_empty() && parsed.result_source.is_none() {
            return ReplOutput::message(String::new());
        }

        let candidate_items = merge_items(&self.items, parsed.items.clone());

        let result_source = parsed.result_source.clone();
        let source = self.synthetic_source(
            &candidate_items,
            if parsed.uses_last_value {
                self.hidden_last.as_ref()
            } else {
                None
            },
            result_source.as_deref(),
        );

        let front_end = match analyze_source(&SourceText::new(
            "<repl-submit>",
            self.next_revision(),
            &source,
        )) {
            Ok(front_end) => front_end,
            Err(diagnostics) => return ReplOutput::error(diagnostics.to_string()),
        };

        if let Err(message) =
            infer_environment(&front_end.syntax, &self.runtime.package_manifests())
        {
            return ReplOutput::error(message);
        }

        let output = if result_source.is_some() {
            match self.evaluate_script_source(&source) {
                Ok(value) => {
                    if let Some(result_source) = parsed.result_source.as_deref() {
                        self.hidden_last = Some(stored_last_value(result_source, &value));
                    }
                    self.render_runtime_value(&value)
                }
                Err(message) => return ReplOutput::error(message),
            }
        } else {
            String::new()
        };

        self.items = candidate_items;
        ReplOutput::message(output)
    }

    fn run_file(&mut self, path: &str) -> ReplOutput {
        if path.trim().is_empty() {
            return ReplOutput::error("`:run` requires a file path".to_owned());
        }

        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return ReplOutput::error(error.to_string()),
        };

        let parsed = match parse_external_script(path, &text) {
            Ok(parsed) => parsed,
            Err(message) => return ReplOutput::error(message),
        };

        let items = merge_items(&self.items, parsed.items);
        let source = self.synthetic_source(&items, None, parsed.result_source.as_deref());

        let front_end = match analyze_source(&SourceText::new(path, self.next_revision(), &source))
        {
            Ok(front_end) => front_end,
            Err(diagnostics) => return ReplOutput::error(diagnostics.to_string()),
        };

        if let Err(message) =
            infer_environment(&front_end.syntax, &self.runtime.package_manifests())
        {
            return ReplOutput::error(message);
        }

        match self.evaluate_script_source(&source) {
            Ok(value) => ReplOutput::message(self.render_runtime_value(&value)),
            Err(message) => ReplOutput::error(message),
        }
    }

    fn type_of(&self, raw_expr: &str) -> ReplOutput {
        if raw_expr.trim().is_empty() {
            return ReplOutput::error("`:type` requires an expression".to_owned());
        }

        let rewritten = rewrite_last_shorthand(raw_expr);
        let source = self.synthetic_source(
            &self.items,
            rewritten
                .uses_last_value
                .then_some(self.hidden_last.as_ref())
                .flatten(),
            Some(&rewritten.source),
        );
        let front_end = match analyze_source(&SourceText::new("<repl-type>", 1, &source)) {
            Ok(front_end) => front_end,
            Err(diagnostics) => return ReplOutput::error(diagnostics.to_string()),
        };

        match infer_environment(&front_end.syntax, &self.runtime.package_manifests()) {
            Ok(environment) => {
                let ty = environment
                    .result
                    .map(|ty| ty.render())
                    .unwrap_or_else(|| "Unit".to_owned());
                ReplOutput::message(ty)
            }
            Err(message) => ReplOutput::error(message),
        }
    }

    fn drop(&mut self, raw: &str) -> ReplOutput {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return ReplOutput::error("`:drop` requires a name".to_owned());
        }

        let target = if trimmed == "$" {
            LAST_VALUE_NAME
        } else {
            trimmed
        };
        let before = self.items.len();
        self.items.retain(|item| !item.matches_drop(target));
        let removed_hidden = if target == LAST_VALUE_NAME {
            self.hidden_last.take().is_some()
        } else {
            false
        };

        if before == self.items.len() && !removed_hidden {
            return ReplOutput::error(format!("no interactive item matched `{trimmed}`"));
        }

        self.runtime.clear_artifacts();
        ReplOutput::message(format!("dropped interactive item(s) matching `{trimmed}`"))
    }

    fn list_handles(&self) -> ReplOutput {
        let handles = self.runtime.live_handles();
        if handles.is_empty() {
            return ReplOutput::message("no live handles".to_owned());
        }

        let mut lines = Vec::new();
        for handle in handles {
            if let Some(summary) = self.runtime.describe_handle(handle) {
                lines.push(format!(
                    "{} {} {}",
                    handle.0, summary.type_name, summary.summary
                ));
            }
        }

        ReplOutput::message(lines.join("\n"))
    }

    fn show_handle(&self, raw: &str) -> ReplOutput {
        let handle = match raw.trim().parse::<u64>() {
            Ok(id) => HandleId(id),
            Err(_) => {
                return ReplOutput::error(format!(
                    "handle id must be an integer, received `{raw}`"
                ));
            }
        };

        match self.runtime.describe_handle(handle) {
            Some(summary) => {
                ReplOutput::message(format!("{} {}", summary.type_name, summary.summary))
            }
            None => ReplOutput::error(format!("handle {} was not found", handle.0)),
        }
    }

    fn snapshot(&self, name: &str) -> ReplOutput {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return ReplOutput::error("`:snapshot` requires a name".to_owned());
        }

        let root = match snapshot_root() {
            Ok(path) => path,
            Err(message) => return ReplOutput::error(message),
        };
        if let Err(error) = fs::create_dir_all(&root) {
            return ReplOutput::error(error.to_string());
        }

        let path = root.join(format!("{trimmed}.vox"));
        let source = self.synthetic_source(&self.items, self.hidden_last.as_ref(), None);
        match fs::write(&path, source) {
            Ok(()) => ReplOutput::message(format!("saved snapshot `{trimmed}`")),
            Err(error) => ReplOutput::error(error.to_string()),
        }
    }

    fn restore(&mut self, name: &str) -> ReplOutput {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return ReplOutput::error("`:restore` requires a name".to_owned());
        }

        let root = match snapshot_root() {
            Ok(path) => path,
            Err(message) => return ReplOutput::error(message),
        };
        let path = root.join(format!("{trimmed}.vox"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => {
                return ReplOutput::error(format!("snapshot `{trimmed}` was not found"));
            }
        };

        let front_end = match analyze_source(&SourceText::new(path_to_label(&path), 1, &text)) {
            Ok(front_end) => front_end,
            Err(diagnostics) => return ReplOutput::error(diagnostics.to_string()),
        };

        if !matches!(front_end.header.kind, ModuleKind::Script { .. }) {
            return ReplOutput::error("snapshot must contain a script state".to_owned());
        }

        if let Err(message) =
            infer_environment(&front_end.syntax, &self.runtime.package_manifests())
        {
            return ReplOutput::error(message);
        }

        let restored = normalize_items(rebuild_items_from_unit(&text, &front_end.syntax));
        let (hidden_last, items): (Vec<_>, Vec<_>) =
            restored.into_iter().partition(StoredItem::is_hidden_last);
        self.items = items;
        self.hidden_last = hidden_last.into_iter().next();
        self.runtime.clear_artifacts();
        ReplOutput::message(format!("restored snapshot `{trimmed}`"))
    }

    fn current_environment(&self, include_hidden_last: bool) -> Result<TypeEnvironment, String> {
        let source = self.synthetic_source(
            &self.items,
            if include_hidden_last {
                self.hidden_last.as_ref()
            } else {
                None
            },
            None,
        );
        let front_end = analyze_source(&SourceText::new("<repl-env>", 1, &source))
            .map_err(|diagnostics| diagnostics.to_string())?;
        infer_environment(&front_end.syntax, &self.runtime.package_manifests())
    }

    fn synthetic_source(
        &self,
        items: &[StoredItem],
        hidden_last: Option<&StoredItem>,
        result: Option<&str>,
    ) -> String {
        let mut source = format!("script {REPL_MODULE};\n");
        for item in items {
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

    fn evaluate_script_source(&mut self, source: &str) -> Result<RuntimeValue, String> {
        let revision = self.next_revision();
        self.runtime.clear_artifacts();
        let artifact_id = self
            .runtime
            .load_script(SourceText::new("<repl-eval>", revision, source), None)
            .map_err(|error| error.to_string())?;
        self.runtime
            .run_script(artifact_id, &[])
            .map_err(|error| error.to_string())
    }

    fn render_runtime_value(&self, value: &RuntimeValue) -> String {
        match value {
            RuntimeValue::Inline(value) => render_inline_value(value),
            RuntimeValue::Handle(handle) => match self.runtime.describe_handle(*handle) {
                Some(summary) => {
                    format!(
                        "<{} handle={}> {}",
                        summary.type_name, handle.0, summary.summary
                    )
                }
                None => format!("<handle {}>", handle.0),
            },
        }
    }

    fn next_revision(&mut self) -> u64 {
        self.next_source_revision += 1;
        self.next_source_revision
    }
}

fn render_help() -> String {
    [
        ":help              - show a brief description of each REPL command",
        ":quit              - exit the REPL",
        ":reset             - clear interactive state",
        ":clear             - clear the screen",
        ":env               - show visible imports, bindings, and functions",
        ":snapshot <name>   - save the current state as a named snapshot",
        ":restore <name>    - restore a previously saved snapshot",
        ":run <file>        - run a script file in the current state",
        ":show <handle>     - show lightweight metadata for a handle",
        ":type <expr>       - show the inferred type of an expression",
        ":handles           - list live handles visible to this session",
        ":drop <name>       - remove a binding or definition from interactive state",
        ":xopt <mode>       - set the default optimization mode (NOpt, IOpt, SOpt)",
    ]
    .join("\n")
}

fn parse_submission(raw: &str) -> Result<ParsedSubmission, String> {
    parse_script_fragment("<repl-submit>", raw)
}

fn parse_external_script(path: &str, raw: &str) -> Result<ParsedSubmission, String> {
    let front_end = analyze_source(&SourceText::new(path, 1, raw)).map_err(|d| d.to_string())?;
    if !matches!(front_end.header.kind, ModuleKind::Script { .. }) {
        return Err("`:run` requires a script file".to_owned());
    }
    Ok(ParsedSubmission {
        items: rebuild_items_from_unit(raw, &front_end.syntax),
        result_source: front_end
            .syntax
            .result
            .as_ref()
            .map(|expr| slice_source(raw, expr)),
        uses_last_value: false,
    })
}

fn parse_script_fragment(path: &str, raw: &str) -> Result<ParsedSubmission, String> {
    let rewritten = rewrite_last_shorthand(raw);
    let wrapped = format!("script {REPL_MODULE};\n{}\n", rewritten.source);
    let front_end = analyze_source(&SourceText::new(path, 1, &wrapped))
        .map_err(|diagnostics| diagnostics.to_string())?;
    Ok(ParsedSubmission {
        items: rebuild_items_from_unit(&wrapped, &front_end.syntax),
        result_source: front_end
            .syntax
            .result
            .as_ref()
            .map(|expr| slice_source(&wrapped, expr)),
        uses_last_value: rewritten.uses_last_value,
    })
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
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escape = false;
    let mut uses_last_value = false;

    while let Some(ch) = chars.next() {
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

fn render_inline_value(value: &vox_core::value::InlineValue) -> String {
    match value {
        vox_core::value::InlineValue::Int(value) => value.to_string(),
        vox_core::value::InlineValue::Float(value) => value.to_string(),
        vox_core::value::InlineValue::Bool(value) => value.to_string(),
        vox_core::value::InlineValue::String(value) => value.clone(),
        vox_core::value::InlineValue::Tuple(values) => match values.as_slice() {
            [] => "()".to_owned(),
            [single] => format!("({},)", render_inline_value(single)),
            _ => format!(
                "({})",
                values
                    .iter()
                    .map(render_inline_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        vox_core::value::InlineValue::Null => "null".to_owned(),
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

fn render_environment(environment: &TypeEnvironment) -> String {
    let mut lines = Vec::new();

    lines.push("Imports:".to_owned());
    if environment.imports.is_empty() {
        lines.push("  <none>".to_owned());
    } else {
        lines.extend(
            environment
                .imports
                .iter()
                .map(|import| format!("  {import}")),
        );
    }

    lines.push("Bindings:".to_owned());
    if environment.bindings.is_empty() {
        lines.push("  <none>".to_owned());
    } else {
        lines.extend(environment.bindings.iter().map(|binding| {
            let mutability = if binding.mutable { "var" } else { "val" };
            let name = if binding.name == LAST_VALUE_NAME {
                "$"
            } else {
                binding.name.as_str()
            };
            format!("  {mutability} {name}: {}", binding.ty.render())
        }));
    }

    lines.push("Functions:".to_owned());
    if environment.functions.is_empty() {
        lines.push("  <none>".to_owned());
    } else {
        lines.extend(environment.functions.iter().map(|function| {
            let head = if function.evil { "evil fun" } else { "fun" };
            let parameters = function
                .parameters
                .iter()
                .map(|parameter| {
                    if parameter.has_default {
                        format!("{}: {} = ...", parameter.name, parameter.ty.render())
                    } else {
                        format!("{}: {}", parameter.name, parameter.ty.render())
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "  {head} {}({parameters}): {}",
                function.name,
                function.return_type.render()
            )
        }));
    }

    lines.join("\n")
}

fn language_keywords() -> Vec<String> {
    [
        "as", "dyn", "econ", "else", "evil", "false", "for", "fun", "if", "import", "in", "is",
        "null", "package", "panic", "param", "private", "public", "return", "script", "true",
        "val", "var", "when",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn extend_manifest_symbols(symbols: &mut Vec<String>, manifest: &PackageManifest) {
    let package = manifest.package.as_str();
    symbols.push(package.clone());

    for function in &manifest.functions {
        symbols.push(format!("{package}.{}", function.name));
    }

    for ty in &manifest.types {
        symbols.push(format!("{package}.{}", ty.name.name));
    }
}

fn snapshot_root() -> Result<PathBuf, String> {
    if cfg!(windows) {
        let Some(appdata) = env::var_os("APPDATA") else {
            return Err("APPDATA is not set".to_owned());
        };
        Ok(PathBuf::from(appdata).join("vox-repl").join("snapshots"))
    } else {
        Ok(PathBuf::from("/tmp/vox-repl/snapshots"))
    }
}

fn available_snapshot_names() -> Vec<String> {
    let Ok(root) = snapshot_root() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("vox") => path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned),
                _ => None,
            }
        })
        .collect()
}

fn path_to_label(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn normalize_error_message(message: String) -> String {
    let mut normalized = message.trim().to_owned();

    if let Some(stripped) = normalized.strip_prefix("compilation failed:\n") {
        normalized = stripped.to_owned();
    }
    if let Some(stripped) = normalized.strip_prefix("script execution failed: ") {
        normalized = stripped.to_owned();
    }

    let lines = normalized
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line
                .trim()
                .strip_prefix("Error: ")
                .or_else(|| line.trim().strip_prefix("error: "))
                .unwrap_or(line.trim());
            format!("TypeError: {line}")
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        "TypeError".to_owned()
    } else {
        lines.join("\n")
    }
}
