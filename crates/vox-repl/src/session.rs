use std::{fs, str::FromStr};

use vox_core::{opt::OptimizationLevel, source::SourceText};
use vox_runtime::Runtime;

use crate::command::ReplCommand;

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
            ReplCommand::Load(path) => self.load_file(&path),
            ReplCommand::Reload => {
                let Some(path) = self.last_loaded_file.clone() else {
                    return ReplOutput::Message("no file has been loaded yet".to_owned());
                };

                self.load_file(&path)
            }
            ReplCommand::Run(_) => ReplOutput::Message(
                "script execution is blocked until the executable plan evaluator lands".to_owned(),
            ),
            ReplCommand::Handles => {
                let stats = self.runtime.cache_stats();
                ReplOutput::Message(format!("{} live handle(s)", stats.handles))
            }
            ReplCommand::Show(handle) => ReplOutput::Message(format!(
                "handle inspection is not implemented yet for `{handle}`"
            )),
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

    fn load_file(&mut self, path: &str) -> ReplOutput {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return ReplOutput::Message(error.to_string()),
        };

        let source = SourceText::new(path, 1, text);
        match self.runtime.load_script(source, None) {
            Ok(artifact_id) => {
                self.last_loaded_file = Some(path.to_owned());
                ReplOutput::Message(format!("loaded script as artifact {}", artifact_id.0))
            }
            Err(error) => ReplOutput::Message(error.to_string()),
        }
    }
}
