use std::{fs, str::FromStr};

use vox_compiler::front_end::{
    analyze_source,
    ast::{CompilationUnit, TopLevelItem},
};
use vox_core::{
    host::PackageManifest,
    ids::{ArtifactId, HandleId},
    opt::OptimizationLevel,
    source::SourceText,
    value::{InlineValue, RuntimeValue},
};
use vox_runtime::Runtime;

use crate::{CompletionSnapshot, command::ReplCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplOutput {
    Message(String),
    Exit,
}

#[derive(Debug, Default)]
pub struct ReplSession {
    runtime: Runtime,
    staged_inputs: Vec<String>,
    last_loaded_file: Option<String>,
    last_loaded_source: Option<String>,
    last_loaded_artifact: Option<ArtifactId>,
    next_source_revision: u64,
}

impl ReplSession {
    pub fn handle_line(&mut self, line: &str) -> ReplOutput {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return ReplOutput::Message(String::new());
        }

        if trimmed.starts_with(':') {
            return self.handle_command(trimmed);
        }

        self.staged_inputs.push(trimmed.to_owned());
        ReplOutput::Message(format!(
            "staged {} input snippet(s) for incremental compilation",
            self.staged_inputs.len()
        ))
    }

    fn handle_command(&mut self, line: &str) -> ReplOutput {
        match ReplCommand::from_str(line) {
            Ok(command) => self.execute(command),
            Err(message) => ReplOutput::Message(message),
        }
    }

    fn execute(&mut self, command: ReplCommand) -> ReplOutput {
        match command {
            ReplCommand::Help => ReplOutput::Message(
                ":help :quit :reset :list :type :purity :load :reload :run :handles :show :drop :xopt"
                    .to_owned(),
            ),
            ReplCommand::Quit => ReplOutput::Exit,
            ReplCommand::Reset => {
                self.staged_inputs.clear();
                self.last_loaded_file = None;
                self.last_loaded_source = None;
                self.last_loaded_artifact = None;
                self.runtime.clear_artifacts();
                ReplOutput::Message("interactive state cleared".to_owned())
            }
            ReplCommand::List => ReplOutput::Message(format!(
                "{} staged snippet(s), last file: {}",
                self.staged_inputs.len(),
                self.last_loaded_file.as_deref().unwrap_or("<none>")
            )),
            ReplCommand::TypeOf(expr) => ReplOutput::Message(format!(
                "type inspection is not implemented yet for `{expr}`"
            )),
            ReplCommand::Purity(expr) => ReplOutput::Message(format!(
                "purity inspection is not implemented yet for `{expr}`"
            )),
            ReplCommand::Load(path) => self.load_file(&path, false),
            ReplCommand::Reload => {
                let Some(path) = self.last_loaded_file.clone() else {
                    return ReplOutput::Message("no file has been loaded yet".to_owned());
                };

                self.load_file(&path, true)
            }
            ReplCommand::Run(args) => self.run(args),
            ReplCommand::Handles => {
                let stats = self.runtime.cache_stats();
                ReplOutput::Message(format!("{} live handle(s)", stats.handles))
            }
            ReplCommand::Show(handle) => self.show_handle(&handle),
            ReplCommand::Drop(name) => {
                self.staged_inputs.retain(|snippet| !snippet.contains(&name));
                ReplOutput::Message(format!("dropped staged snippets matching `{name}`"))
            }
            ReplCommand::XOpt(mode) => {
                let xopt = match mode.as_str() {
                    "NOpt" => OptimizationLevel::NOpt,
                    "IOpt" => OptimizationLevel::IOpt,
                    "SOpt" => OptimizationLevel::SOpt,
                    _ => {
                        return ReplOutput::Message(format!(
                            "unknown optimization mode `{mode}`"
                        ));
                    }
                };
                self.runtime.set_default_xopt(xopt);
                ReplOutput::Message(format!("default optimization mode set to {mode}"))
            }
        }
    }

    fn load_file(&mut self, path: &str, reload: bool) -> ReplOutput {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return ReplOutput::Message(error.to_string()),
        };

        let source = SourceText::new(path, self.next_revision(), &text);
        let result = if reload {
            if let Some(artifact_id) = self.last_loaded_artifact {
                self.runtime
                    .reload_script(artifact_id, source)
                    .map(|()| artifact_id)
            } else {
                self.runtime.load_script(source, None)
            }
        } else {
            self.runtime.load_script(source, None)
        };

        match result {
            Ok(artifact_id) => {
                self.last_loaded_file = Some(path.to_owned());
                self.last_loaded_source = Some(text);
                self.last_loaded_artifact = Some(artifact_id);
                let action = if reload { "reloaded" } else { "loaded" };
                ReplOutput::Message(format!("{action} script as artifact {}", artifact_id.0))
            }
            Err(error) => ReplOutput::Message(error.to_string()),
        }
    }

    fn run(&mut self, args: Vec<String>) -> ReplOutput {
        let arguments = args
            .iter()
            .map(|arg| parse_runtime_argument(arg))
            .collect::<Vec<_>>();

        let artifact_id = if !self.staged_inputs.is_empty() {
            let source = SourceText::new(
                "<repl>",
                self.next_revision(),
                self.synthetic_script_source(),
            );
            match self.runtime.load_script(source, None) {
                Ok(artifact_id) => artifact_id,
                Err(error) => return ReplOutput::Message(error.to_string()),
            }
        } else if let Some(artifact_id) = self.last_loaded_artifact {
            artifact_id
        } else {
            return ReplOutput::Message(
                "nothing to run yet; enter Vox code or `:load <file>` first".to_owned(),
            );
        };

        match self.runtime.run_script(artifact_id, &arguments) {
            Ok(value) => ReplOutput::Message(self.render_runtime_value(&value)),
            Err(error) => ReplOutput::Message(error.to_string()),
        }
    }

    fn show_handle(&self, raw: &str) -> ReplOutput {
        let handle = match raw.parse::<u64>() {
            Ok(id) => HandleId(id),
            Err(_) => {
                return ReplOutput::Message(format!(
                    "handle id must be an integer, received `{raw}`"
                ));
            }
        };

        match self.runtime.describe_handle(handle) {
            Some(summary) => {
                ReplOutput::Message(format!("{} {}", summary.type_name, summary.summary))
            }
            None => ReplOutput::Message(format!("handle {} was not found", handle.0)),
        }
    }

    fn synthetic_script_source(&self) -> String {
        let mut source = String::from("script repl.session;\n");
        source.push_str(&self.staged_inputs.join("\n"));
        source
    }

    fn next_revision(&mut self) -> u64 {
        self.next_source_revision += 1;
        self.next_source_revision
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

    pub fn completion_snapshot(&self) -> CompletionSnapshot {
        let mut snapshot = CompletionSnapshot {
            commands: vec![
                ":help".to_owned(),
                ":quit".to_owned(),
                ":reset".to_owned(),
                ":list".to_owned(),
                ":type".to_owned(),
                ":purity".to_owned(),
                ":load".to_owned(),
                ":reload".to_owned(),
                ":run".to_owned(),
                ":handles".to_owned(),
                ":show".to_owned(),
                ":drop".to_owned(),
                ":xopt".to_owned(),
            ],
            command_args: vec!["true".to_owned(), "false".to_owned(), "null".to_owned()],
            xopts: vec!["NOpt".to_owned(), "IOpt".to_owned(), "SOpt".to_owned()],
            handles: self
                .runtime
                .live_handles()
                .into_iter()
                .map(|handle| handle.0.to_string())
                .collect(),
            symbols: language_keywords(),
        };

        for manifest in self.runtime.package_manifests() {
            extend_manifest_symbols(&mut snapshot.symbols, &manifest);
        }

        if !self.staged_inputs.is_empty() {
            collect_visible_symbols(
                &mut snapshot.symbols,
                &self.synthetic_script_source(),
                "<repl-completion>",
            );
        }

        if let Some(source) = &self.last_loaded_source {
            let path = self
                .last_loaded_file
                .as_deref()
                .unwrap_or("<loaded-script>");
            collect_visible_symbols(&mut snapshot.symbols, source, path);
        }

        snapshot.symbols.sort();
        snapshot.symbols.dedup();
        snapshot.command_args.sort();
        snapshot.command_args.dedup();
        snapshot
    }
}

fn parse_runtime_argument(raw: &str) -> RuntimeValue {
    if raw == "null" {
        return RuntimeValue::Inline(InlineValue::Null);
    }
    if raw == "true" {
        return RuntimeValue::Inline(InlineValue::Bool(true));
    }
    if raw == "false" {
        return RuntimeValue::Inline(InlineValue::Bool(false));
    }
    if let Ok(value) = raw.parse::<i64>() {
        return RuntimeValue::Inline(InlineValue::Int(value));
    }
    if let Ok(value) = raw.parse::<f64>() {
        return RuntimeValue::Inline(InlineValue::Float(value));
    }

    RuntimeValue::Inline(InlineValue::String(raw.to_owned()))
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

fn collect_visible_symbols(symbols: &mut Vec<String>, text: &str, path: &str) {
    let source = SourceText::new(path, 1, text);
    let Ok(front_end) = analyze_source(&source) else {
        return;
    };

    collect_unit_symbols(symbols, &front_end.syntax);
}

fn collect_unit_symbols(symbols: &mut Vec<String>, unit: &CompilationUnit) {
    let module = unit.header.module.as_str();
    symbols.push(module.clone());

    for item in &unit.items {
        match item {
            TopLevelItem::Import(import) => {
                symbols.push(import.module.to_source_string());
            }
            TopLevelItem::Param(param) => {
                symbols.push(param.name.clone());
            }
            TopLevelItem::Value(value) => {
                symbols.push(value.name.clone());
                symbols.push(format!("{module}.{}", value.name));
            }
            TopLevelItem::Function(function) => {
                symbols.push(function.name.clone());
                symbols.push(format!("{module}.{}", function.name));
            }
        }
    }
}
