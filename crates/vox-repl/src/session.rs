use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use vox_core::{
    ids::HandleId,
    opt::OptimizationLevel,
    value::{InlineValue, RuntimeValue},
};
use vox_runtime::{EmbeddedRunner, InteractiveSession, TypeEnvironment};

use crate::{CompletionSnapshot, command::ReplCommand};

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

#[derive(Debug, Clone)]
pub struct ReplSession {
    runtime: InteractiveSession<EmbeddedRunner>,
}

impl Default for ReplSession {
    fn default() -> Self {
        Self {
            runtime: InteractiveSession::new(EmbeddedRunner::default()),
        }
    }
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

        match self.runtime.evaluate_submission(line) {
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
            handles,
            symbols,
        };

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
            ReplCommand::Reset => match self.runtime.reset() {
                Ok(()) => ReplOutput::message("interactive state cleared".to_owned()),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::Clear => ReplOutput::message("\x1b[2J\x1b[H".to_owned()),
            ReplCommand::Env => match self.runtime.current_environment(true) {
                Ok(environment) => ReplOutput::message(render_environment(&environment)),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::Snapshot(name) => self.snapshot(&name),
            ReplCommand::Restore(name) => self.restore(&name),
            ReplCommand::TypeOf(expr) => match self.runtime.type_of(&expr) {
                Ok(ty) => ReplOutput::message(ty.render()),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::Run(path) => self.run_file(&path),
            ReplCommand::Handles => self.list_handles(),
            ReplCommand::Show(handle) => self.show_handle(&handle),
            ReplCommand::Drop(name) => match self.runtime.drop_item(&name) {
                Ok(true) => ReplOutput::message(format!(
                    "dropped interactive item(s) matching `{}`",
                    name.trim()
                )),
                Ok(false) => ReplOutput::error(format!(
                    "no interactive item matched `{}`",
                    name.trim()
                )),
                Err(error) => ReplOutput::error(error.to_string()),
            },
            ReplCommand::XOpt(mode) => {
                let xopt = match mode.as_str() {
                    "NOpt" => OptimizationLevel::NOpt,
                    "IOpt" => OptimizationLevel::IOpt,
                    "SOpt" => OptimizationLevel::SOpt,
                    _ => {
                        return ReplOutput::error(format!(
                            "unknown optimization mode `{mode}`"
                        ));
                    }
                };
                match self.runtime.set_default_xopt(xopt) {
                    Ok(()) => {
                        ReplOutput::message(format!("default optimization mode set to {mode}"))
                    }
                    Err(error) => ReplOutput::error(error.to_string()),
                }
            }
        }
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
        match fs::write(&path, self.runtime.snapshot_source()) {
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

fn render_inline_value(value: &InlineValue) -> String {
    match value {
        InlineValue::Int(value) => value.to_string(),
        InlineValue::Float(value) => value.to_string(),
        InlineValue::Bool(value) => value.to_string(),
        InlineValue::String(value) => value.clone(),
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
        InlineValue::Null => "null".to_owned(),
    }
}

fn render_environment(environment: &TypeEnvironment) -> String {
    let mut lines = Vec::new();

    lines.push("Imports:".to_owned());
    if environment.imports.is_empty() {
        lines.push("  <none>".to_owned());
    } else {
        lines.extend(environment.imports.iter().map(|import| format!("  {import}")));
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
