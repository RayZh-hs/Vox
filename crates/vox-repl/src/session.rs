use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use vox_core::{
    ids::{HandleId, SessionId},
    opt::OptimizationLevel,
    value::{InlineValue, RuntimeValue},
};
use vox_runtime::{
    EmbeddedRunner, InteractiveSession, OptimizationDumpKind, OptimizationStatus, RuntimeRunner,
    SessionOpenMode, SessionOpenRequest, SessionSelector, SessionSummary, TypeEnvironment,
};

use crate::{
    CompletionSnapshot,
    command::{OptCommand, ReplCommand, SessionCommand},
    editing::{build_edit_buffer, validate_edited_symbols},
    editor::{EditOutcome, edit_chunk, view_text},
};

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

#[derive(Debug)]
pub struct ReplSession<R: RuntimeRunner = EmbeddedRunner> {
    runner: R,
    runtime: InteractiveSession<R>,
}

impl Default for ReplSession<EmbeddedRunner> {
    fn default() -> Self {
        Self::with_runner(EmbeddedRunner::default())
    }
}

impl<R: RuntimeRunner> ReplSession<R> {
    pub fn with_runner(runner: R) -> Self {
        Self::with_session_request(
            runner,
            SessionOpenRequest {
                selector: None,
                mode: SessionOpenMode::Create,
            },
        )
    }

    pub fn with_session_request(runner: R, request: SessionOpenRequest) -> Self {
        let runtime = InteractiveSession::open(runner.clone(), request)
            .expect("interactive session should open");
        Self { runner, runtime }
    }

    pub fn handle_line(&mut self, line: &str) -> ReplOutput {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return ReplOutput::message(String::new());
        }

        if trimmed.starts_with(':') {
            return self.handle_command(trimmed);
        }

        match self.runtime.eval(line) {
            Ok(Some(value)) => ReplOutput::message(self.render_runtime_value(&value)),
            Ok(None) => ReplOutput::message(String::new()),
            Err(error) => ReplOutput::error(error.to_string()),
        }
    }

    pub fn completion_snapshot(&self) -> CompletionSnapshot {
        let runtime = self.runtime.completion();
        let (handles, symbols) = match runtime {
            Ok(runtime) => (runtime.handles, runtime.symbols),
            Err(_) => (Vec::new(), Vec::new()),
        };
        let sessions = self
            .runtime
            .list_sessions()
            .unwrap_or_default()
            .into_iter()
            .flat_map(|session| session_completion_keys(&session))
            .collect::<Vec<_>>();

        let mut snapshot = CompletionSnapshot {
            commands: vec![
                ":help".to_owned(),
                ":quit".to_owned(),
                ":reset".to_owned(),
                ":clear".to_owned(),
                ":env".to_owned(),
                ":chunk".to_owned(),
                ":edit".to_owned(),
                ":snapshot".to_owned(),
                ":restore".to_owned(),
                ":run".to_owned(),
                ":mount".to_owned(),
                ":show".to_owned(),
                ":type".to_owned(),
                ":handles".to_owned(),
                ":drop".to_owned(),
                ":opt".to_owned(),
                ":session".to_owned(),
            ],
            snapshots: available_snapshot_names(),
            xopts: vec!["NOpt".to_owned(), "IOpt".to_owned(), "SOpt".to_owned()],
            opt_commands: vec!["get".to_owned(), "set".to_owned(), "dump".to_owned()],
            handles,
            sessions,
            session_commands: vec![
                "connect".to_owned(),
                "new".to_owned(),
                "reserve".to_owned(),
                "list".to_owned(),
                "transfer".to_owned(),
            ],
            symbols,
        };

        snapshot.symbols.push("module".to_owned());
        snapshot.snapshots.sort();
        snapshot.snapshots.dedup();
        snapshot.symbols.sort();
        snapshot.symbols.dedup();
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
            ReplCommand::Reset => match self.runtime.reset() {
                Ok(()) => ReplOutput::message("interactive state cleared".to_owned()),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::Clear => ReplOutput::message("\x1b[2J\x1b[H".to_owned()),
            ReplCommand::Env => match self.runtime.current_environment(true) {
                Ok(environment) => ReplOutput::message(render_environment(&environment)),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::Chunk => self.edit_submission(Vec::new()),
            ReplCommand::Edit(symbols) => self.edit_submission(parse_symbol_list(&symbols)),
            ReplCommand::Snapshot(name) => self.snapshot(&name),
            ReplCommand::Restore(name) => self.restore(&name),
            ReplCommand::TypeOf(expr) => match self.runtime.type_of(&expr) {
                Ok(ty) => ReplOutput::message(ty.render()),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::Run(path) => self.run_file(&path),
            ReplCommand::Mount(paths) => self.mount_libraries(&paths),
            ReplCommand::Handles => self.list_handles(),
            ReplCommand::Show(handle) => self.show_handle(&handle),
            ReplCommand::Drop(name) => match self.runtime.drop_item(&name) {
                Ok(true) => ReplOutput::message(format!(
                    "dropped interactive item(s) matching `{}`",
                    name.trim()
                )),
                Ok(false) => {
                    ReplOutput::error(format!("no interactive item matched `{}`", name.trim()))
                }
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::Opt(command) => self.handle_opt_command(command),
            ReplCommand::Session(command) => self.handle_session_command(command),
        }
    }

    fn handle_opt_command(&mut self, command: OptCommand) -> ReplOutput {
        match command {
            OptCommand::Get(object) => match self.runtime.optimization_status(object.as_deref()) {
                Ok(statuses) => ReplOutput::message(render_optimization_statuses(&statuses)),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            OptCommand::Set { mode, objects } => {
                let Some(xopt) = parse_optimization_level(&mode) else {
                    return ReplOutput::error(format!("unknown optimization mode `{mode}`"));
                };
                match self.runtime.set_optimization(xopt, &objects) {
                    Ok(()) if objects.is_empty() => {
                        ReplOutput::message(format!("default optimization mode set to {mode}"))
                    }
                    Ok(()) => ReplOutput::message(format!(
                        "optimization mode set to {mode} for {}",
                        render_backticked_names(&objects)
                    )),
                    Err(error) => ReplOutput::error(error.to_string()),
                }
            }
            OptCommand::Dump(object) => {
                let object = object.unwrap_or_else(|| "module".to_owned());
                let (kind, object) = parse_dump_target(&object);
                match self.runtime.optimization_dump(&object, kind) {
                    Ok(Some(dump)) => match view_text(
                        &format!("{}-{}", render_dump_kind(kind).to_ascii_lowercase(), object),
                        &dump.text,
                    ) {
                        Ok(true) => ReplOutput::message(format!(
                            "opened {} dump for `{}`",
                            render_dump_kind(kind),
                            object
                        )),
                        Ok(false) => ReplOutput::message(dump.text),
                        Err(error) => ReplOutput::error(error),
                    },
                    Ok(None) => ReplOutput::error(format!(
                        "no {} dump is available for `{}`",
                        render_dump_kind(kind),
                        object
                    )),
                    Err(error) => ReplOutput::error(error.to_string()),
                }
            }
        }
    }

    fn handle_session_command(&mut self, command: SessionCommand) -> ReplOutput {
        match command {
            SessionCommand::Connect(target) => {
                let selector = match parse_session_selector(&target) {
                    Ok(selector) => selector,
                    Err(error) => return ReplOutput::error(error),
                };
                let next = match InteractiveSession::attach(self.runner.clone(), selector) {
                    Ok(session) => session,
                    Err(error) => return ReplOutput::error(error.to_string()),
                };
                self.replace_runtime(next, "connected to")
            }
            SessionCommand::New(name) => {
                let next = match name {
                    Some(name) => match InteractiveSession::create_named(self.runner.clone(), name)
                    {
                        Ok(session) => session,
                        Err(error) => return ReplOutput::error(error.to_string()),
                    },
                    None => match InteractiveSession::new(self.runner.clone()) {
                        Ok(session) => session,
                        Err(error) => return ReplOutput::error(error.to_string()),
                    },
                };
                self.replace_runtime(next, "opened")
            }
            SessionCommand::Reserve => self.toggle_session_reserve(),
            SessionCommand::List => self.list_sessions(),
            SessionCommand::Transfer {
                binding,
                source,
                alias,
            } => self.transfer_session_binding(&binding, &source, alias.as_deref()),
        }
    }

    fn transfer_session_binding(
        &mut self,
        binding: &str,
        source: &str,
        alias: Option<&str>,
    ) -> ReplOutput {
        let selector = match parse_session_selector(source) {
            Ok(selector) => selector,
            Err(error) => return ReplOutput::error(error),
        };
        let target_name = match transfer_target_name(binding, alias) {
            Ok(name) => name,
            Err(error) => return ReplOutput::error(error),
        };
        let source_id = match self
            .runtime
            .transfer_binding_from(selector, binding, &target_name)
        {
            Ok(source_id) => source_id,
            Err(error) => return ReplOutput::error(error.to_string()),
        };
        ReplOutput::message(format!(
            "transferred `{}` from session {} as `{}`",
            binding.trim(),
            source_id.0,
            target_name
        ))
    }

    fn run_file(&mut self, path: &str) -> ReplOutput {
        if path.trim().is_empty() {
            return ReplOutput::error("`:run` requires a file path".to_owned());
        }

        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return ReplOutput::error(error.to_string()),
        };

        match self.runtime.run_script_text(path, &text) {
            Ok(value) => ReplOutput::message(self.render_runtime_value(&value)),
            Err(error) => ReplOutput::error(error.to_string()),
        }
    }

    fn mount_libraries(&mut self, paths: &[String]) -> ReplOutput {
        if paths.is_empty() {
            return ReplOutput::error("`:mount` requires at least one path".to_owned());
        }

        let mut messages = Vec::new();
        for path in paths {
            let p = Path::new(path);
            match mount_at_path(&self.runner, p) {
                Ok(ids) => {
                    let count = ids.len();
                    messages.push(format!(
                        "mounted {} ({} librar{})",
                        path,
                        count,
                        if count == 1 { "y" } else { "ies" }
                    ));
                }
                Err(error) => {
                    messages.push(format!("failed to mount {}: {error}", path));
                }
            }
        }
        ReplOutput::message(messages.join("\n"))
    }

    fn edit_submission(&mut self, symbols: Vec<String>) -> ReplOutput {
        let snapshot = match self.runtime.snapshot_source() {
            Ok(snapshot) => snapshot,
            Err(error) => return ReplOutput::error(error.to_string()),
        };
        let initial = match build_edit_buffer(&snapshot, &symbols) {
            Ok(initial) => initial,
            Err(error) => return ReplOutput::error(error),
        };
        let label = if symbols.is_empty() {
            "new chunk".to_owned()
        } else {
            format!("definitions {}", render_backticked_names(&symbols))
        };

        let edited = match edit_chunk(&label, &initial) {
            Ok(EditOutcome::Submitted(text)) => text,
            Ok(EditOutcome::Cancelled) => {
                return ReplOutput::message("editor cancelled".to_owned());
            }
            Err(error) => return ReplOutput::error(error),
        };

        if edited.trim().is_empty() {
            return ReplOutput::message("no chunk submitted".to_owned());
        }

        if let Err(error) = validate_edited_symbols(&edited, &symbols) {
            return ReplOutput::error(error);
        }

        match self.runtime.evaluate_submission(&edited) {
            Ok(Some(value)) => ReplOutput::message(self.render_runtime_value(&value)),
            Ok(None) => ReplOutput::message("chunk submitted".to_owned()),
            Err(error) => ReplOutput::error(error.to_string()),
        }
    }

    fn list_handles(&self) -> ReplOutput {
        let handles = match self.runtime.live_handles() {
            Ok(handles) => handles,
            Err(error) => return ReplOutput::error(error.to_string()),
        };
        if handles.is_empty() {
            return ReplOutput::message("no live handles".to_owned());
        }

        let mut lines = Vec::new();
        for handle in handles {
            if let Ok(Some(summary)) = self.runtime.describe_handle(handle) {
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
            Ok(Some(summary)) => {
                ReplOutput::message(format!("{} {}", summary.type_name, summary.summary))
            }
            Ok(None) => ReplOutput::error(format!("handle {} was not found", handle.0)),
            Err(error) => ReplOutput::error(error.to_string()),
        }
    }

    fn list_sessions(&self) -> ReplOutput {
        let sessions = match self.runtime.list_sessions() {
            Ok(sessions) => sessions,
            Err(error) => return ReplOutput::error(error.to_string()),
        };
        if sessions.is_empty() {
            return ReplOutput::message("no sessions".to_owned());
        }

        let current = self.runtime.id();
        let lines = sessions
            .iter()
            .map(|session| render_session_row(session, session.id == current))
            .collect::<Vec<_>>();
        ReplOutput::message(lines.join("\n"))
    }

    fn toggle_session_reserve(&mut self) -> ReplOutput {
        let summary = match self.current_session_summary() {
            Ok(summary) => summary,
            Err(error) => return ReplOutput::error(error),
        };
        let next_reserved = !summary.reserved;
        if let Err(error) = self.runtime.set_reserved(next_reserved) {
            return ReplOutput::error(error.to_string());
        }
        let summary = match self.current_session_summary() {
            Ok(summary) => summary,
            Err(error) => return ReplOutput::error(error),
        };
        ReplOutput::message(format!(
            "{} {}",
            if summary.reserved {
                "reserved"
            } else {
                "unreserved"
            },
            render_session_identity(&summary)
        ))
    }

    fn replace_runtime(&mut self, next: InteractiveSession<R>, verb: &str) -> ReplOutput {
        let previous = std::mem::replace(&mut self.runtime, next);
        drop(previous);
        match self.current_session_summary() {
            Ok(summary) => {
                ReplOutput::message(format!("{verb} {}", render_session_status(&summary)))
            }
            Err(_) => ReplOutput::message(format!("{verb} session {}", self.runtime.id().0)),
        }
    }

    fn current_session_summary(&self) -> Result<SessionSummary, String> {
        let current_id = self.runtime.id();
        let sessions = self
            .runtime
            .list_sessions()
            .map_err(|error| normalize_error_message(error.to_string()))?;
        sessions
            .into_iter()
            .find(|session| session.id == current_id)
            .ok_or_else(|| format!("current session {} was not found", current_id.0))
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
        match self.runtime.snapshot_source() {
            Ok(snapshot) => match fs::write(&path, snapshot) {
                Ok(()) => ReplOutput::message(format!("saved snapshot `{trimmed}`")),
                Err(error) => ReplOutput::error(error.to_string()),
            },
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

        match self
            .runtime
            .restore_snapshot_source(&path_to_label(&path), &text)
        {
            Ok(()) => ReplOutput::message(format!("restored snapshot `{trimmed}`")),
            Err(error) => ReplOutput::error(error.to_string()),
        }
    }

    fn render_runtime_value(&self, value: &RuntimeValue) -> String {
        match value {
            RuntimeValue::Inline(value) => render_inline_value(value),
            RuntimeValue::Handle(handle) => match self.runtime.describe_handle(*handle) {
                Ok(Some(summary)) => {
                    format!(
                        "<{} handle={}> {}",
                        summary.type_name, handle.0, summary.summary
                    )
                }
                Ok(None) | Err(_) => format!("<handle {}>", handle.0),
            },
        }
    }
}

fn mount_at_path<R: RuntimeRunner>(
    runner: &R,
    path: &Path,
) -> Result<Vec<vox_core::ids::LibraryId>, String> {
    if path.is_dir() {
        runner
            .mount_dir(path)
            .map_err(|error| error.to_string())
    } else {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("vox") => runner
                .mount_vox_file(path)
                .map(|id| vec![id])
                .map_err(|error| error.to_string()),
            Some("voxlib") => runner
                .mount_voxlib_file(path)
                .map(|id| vec![id])
                .map_err(|error| error.to_string()),
            other => Err(format!(
                "unsupported file extension for mounting: {:?}",
                other
            )),
        }
    }
}

fn render_help() -> String {
    [
        ":help                      - show a brief description of each REPL command",
        ":quit                      - exit the REPL",
        ":reset                     - clear interactive state",
        ":clear                     - clear the screen",
        ":env                       - show visible imports, bindings, and functions",
        ":chunk                     - open an editor for a new multi-definition chunk",
        ":edit [symbol ...]         - edit stored definitions together as one chunk",
        ":snapshot [name]           - save the current state as a named snapshot",
        ":restore [name]            - restore a previously saved snapshot",
        ":run [file]                - run a script file in the current state",
        ":mount [path ...]          - mount libraries from folders, .vox, or .voxlib files",
        ":show [handle]             - show lightweight metadata for a handle",
        ":type [expr]               - show the inferred type of an expression",
        ":handles                   - list live handles visible to this session",
        ":drop [name]               - remove a binding or definition from interactive state",
        ":opt get [object]          - show optimization state for objects",
        ":opt set [mode] [object...] - set default or per-object optimization mode",
        ":opt dump [object]         - print a MIR dump; use wasm:module for wasm bytes",
        ":session connect (target)  - attach to an existing session by id or name",
        ":session new [name]        - create a fresh anonymous or named session",
        ":session reserve           - toggle whether the current session is retained at 0 users",
        ":session transfer [binding] from [session] [as name] - copy a live binding into this session",
        ":session list              - list available sessions",
    ]
    .join("\n")
}

fn transfer_target_name(binding: &str, alias: Option<&str>) -> Result<String, String> {
    if let Some(alias) = alias {
        let alias = alias.trim();
        if alias.is_empty() {
            return Err("transfer target name must not be empty".to_owned());
        }
        return Ok(alias.to_owned());
    }

    let binding = binding.trim();
    if binding == "$" {
        return Err("transferring `$` requires `as <name>`".to_owned());
    }
    if !binding
        .chars()
        .all(|ch| ch == '_' || ch == '.' || ch.is_ascii_alphanumeric())
    {
        return Err("transfer target must be named with `as <name>`".to_owned());
    }
    Ok(binding
        .rsplit('.')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(binding)
        .to_owned())
}

fn parse_optimization_level(raw: &str) -> Option<OptimizationLevel> {
    match raw.trim() {
        "NOpt" => Some(OptimizationLevel::NOpt),
        "IOpt" => Some(OptimizationLevel::IOpt),
        "SOpt" => Some(OptimizationLevel::SOpt),
        _ => None,
    }
}

fn parse_dump_target(raw: &str) -> (OptimizationDumpKind, String) {
    let trimmed = raw.trim();
    if let Some(object) = trimmed.strip_prefix("wasm:") {
        (OptimizationDumpKind::Wasm, object.trim().to_owned())
    } else if let Some(object) = trimmed.strip_prefix("mir:") {
        (OptimizationDumpKind::Mir, object.trim().to_owned())
    } else {
        (OptimizationDumpKind::Mir, trimmed.to_owned())
    }
}

fn render_dump_kind(kind: OptimizationDumpKind) -> &'static str {
    match kind {
        OptimizationDumpKind::Mir => "MIR",
        OptimizationDumpKind::Wasm => "Wasm",
    }
}

fn render_optimization_statuses(statuses: &[OptimizationStatus]) -> String {
    if statuses.is_empty() {
        return "no optimization objects".to_owned();
    }

    statuses
        .iter()
        .map(|status| {
            let rank = status
                .rank
                .map(|rank| rank.as_str().to_owned())
                .unwrap_or_else(|| "pending".to_owned());
            let artifact = status
                .artifact
                .map(|artifact| artifact.0.to_string())
                .unwrap_or_else(|| "-".to_owned());
            let dumps = [
                status.mir_available.then_some("mir"),
                status.wasm_available.then_some("wasm"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let dumps = if dumps.is_empty() {
                "-".to_owned()
            } else {
                dumps.join(",")
            };
            let mut rendered = format!(
                "{} mode={} rank={} artifact={} dumps={}",
                status.object,
                status.requested.as_str(),
                rank,
                artifact,
                dumps
            );
            if let Some(note) = &status.runtime_note {
                rendered.push_str(" note=");
                rendered.push_str(note);
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_session_selector(raw: &str) -> Result<SessionSelector, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("session id or name must not be empty".to_owned());
    }

    match trimmed.parse::<u64>() {
        Ok(id) => Ok(SessionSelector::Id(SessionId(id))),
        Err(_) => Ok(SessionSelector::Name(trimmed.to_owned())),
    }
}

fn render_session_row(session: &SessionSummary, current: bool) -> String {
    format!(
        "{} {} attached={} reserved={}",
        if current { "*" } else { " " },
        render_session_identity(session),
        session.attached_endpoints,
        if session.reserved { "yes" } else { "no" }
    )
}

fn render_session_identity(session: &SessionSummary) -> String {
    match session.name.as_deref() {
        Some(name) => format!("{} ({name})", session.id.0),
        None => format!("{} (<anonymous>)", session.id.0),
    }
}

fn render_session_status(session: &SessionSummary) -> String {
    format!(
        "{} attached={} reserved={}",
        render_session_identity(session),
        session.attached_endpoints,
        if session.reserved { "yes" } else { "no" }
    )
}

fn parse_symbol_list(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(str::to_owned).collect()
}

fn render_backticked_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn session_completion_keys(session: &SessionSummary) -> Vec<String> {
    let mut keys = vec![session.id.0.to_string()];
    if let Some(name) = session.name.as_ref() {
        keys.push(name.clone());
    }
    keys
}

fn render_inline_value(value: &InlineValue) -> String {
    match value {
        InlineValue::Int(value) => value.to_string(),
        InlineValue::Float(value) => value.to_string(),
        InlineValue::Bool(value) => value.to_string(),
        InlineValue::String(value) => value.clone(),
        InlineValue::Handle(handle) => format!("<handle {}>", handle.0),
        InlineValue::Tuple(values) => match values.as_slice() {
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
        InlineValue::Record(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", render_inline_value(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        InlineValue::Null => "null".to_owned(),
    }
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
                "  {head} {}{}({parameters}): {}",
                function.name,
                if function.generic_parameters.is_empty() {
                    String::new()
                } else {
                    format!(
                        "[{}]",
                        function
                            .generic_parameters
                            .iter()
                            .map(|parameter| format!("{}: {}", parameter.name, parameter.bound))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                function.return_type.render()
            )
        }));
    }

    lines.join("\n")
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

#[derive(Clone, Copy)]
enum ErrorKind {
    Generic,
    Compile,
    Runtime,
}

fn normalize_error_message(message: String) -> String {
    let normalized = message.trim();
    let (kind, body) = if let Some(stripped) = normalized.strip_prefix("compilation failed:\n") {
        (ErrorKind::Compile, stripped)
    } else if let Some(stripped) = normalized.strip_prefix("script execution failed: ") {
        (ErrorKind::Runtime, stripped)
    } else {
        (ErrorKind::Generic, normalized)
    };

    let lines = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| normalize_error_line(kind, line.trim()))
        .collect::<Vec<_>>();

    if lines.is_empty() {
        match kind {
            ErrorKind::Compile => "CompileError".to_owned(),
            ErrorKind::Runtime => "RuntimeError".to_owned(),
            ErrorKind::Generic => "Error".to_owned(),
        }
    } else {
        lines.join("\n")
    }
}

fn normalize_error_line(kind: ErrorKind, line: &str) -> String {
    match kind {
        ErrorKind::Compile => {
            if let Some(stripped) = line
                .strip_prefix("Error: ")
                .or_else(|| line.strip_prefix("error: "))
            {
                format!("SyntaxError: {stripped}")
            } else if has_explicit_error_prefix(line) {
                line.to_owned()
            } else {
                format!("CompileError: {line}")
            }
        }
        ErrorKind::Runtime => {
            if has_explicit_error_prefix(line) {
                line.to_owned()
            } else {
                format!("RuntimeError: {line}")
            }
        }
        ErrorKind::Generic => {
            if has_explicit_error_prefix(line) {
                line.to_owned()
            } else {
                format!("Error: {line}")
            }
        }
    }
}

fn has_explicit_error_prefix(line: &str) -> bool {
    matches!(
        line.split_once(':').map(|(prefix, _)| prefix),
        Some("Error" | "SyntaxError" | "CompileError" | "RuntimeError" | "TypeError")
    )
}
