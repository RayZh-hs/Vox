//! `vox-repl` — Interactive read-eval-print loop for the Vox language.
//!
//! ## Usage
//!
//! ```text
//! vox-repl [OPTIONS] [SCRIPT] [-- SCRIPT_ARGS...]
//! ```
//!
//! When run without arguments, starts an interactive REPL session.
//! When given a script file, runs it and prints the trailing expression
//! value to stderr.  Use `-i` to drop into the REPL after the script has
//! run, and `-s` to suppress the trailing-expression output.
//!
//! Script arguments after `--` are converted to Vox values (integers,
//! floats, booleans, `null`, or strings) and passed as the script's
//! positional parameters.
//!
//! ### Options
//!
//! | Flag | Description |
//! |------|-------------|
//! | `-i`, `--interactive` | Drop into REPL after running the script |
//! | `-s`, `--silent` | Suppress stderr output of trailing expressions |
//! | `--connect ADDR` | Connect to a remote vox-runtime instance (host:port\[@session\]) |
//! | `--new` | Create session if missing (requires `--connect`) |
//! | `-h`, `--help` | Show help message |
//!
//! ### Examples
//!
//! ```sh
//! # Start an interactive REPL
//! vox-repl
//!
//! # Run a script, showing its result on stderr
//! vox-repl hello.vox
//!
//! # Run a script, then drop into the REPL
//! vox-repl -i hello.vox
//!
//! # Run a script silently (no stderr output), then REPL
//! vox-repl -i -s hello.vox
//!
//! # Pass arguments to a parameterised script
//! vox-repl greet.vox -- "Alice" 42
//! ```

use std::{env, error::Error, fs};

use rustyline::{
    Cmd, ConditionalEventHandler, Config, Editor, Event, EventContext, EventHandler, KeyCode,
    KeyEvent, Modifiers, Movement, RepeatCount, history::DefaultHistory,
};
use vox_core::value::{InlineValue, RuntimeValue};
use vox_repl::{CompletionUi, ReplHelper, ReplOutput, ReplSession, TabCompletion};
use vox_runtime::{
    EmbeddedRunner, RemoteRunner, RuntimeRunner, SessionOpenMode, SessionOpenRequest,
    SessionSelector,
};

const PRIMARY_PROMPT: &str = ">>> ";
const CONTINUATION_PROMPT: &str = "... ";
const INDENT: &str = "    ";

fn main() -> Result<(), Box<dyn Error>> {
    let CliArgs { connect, script_path, interactive, silent, script_args } = parse_cli_args()?;
    let runner = match connect {
        Some(spec) => RunnerChoice::Remote {
            runner: RemoteRunner::connect(spec.addr)?,
            session: spec.session,
        },
        None => RunnerChoice::Embedded(EmbeddedRunner::default()),
    };

    let should_repl = interactive || script_path.is_none();
    match runner {
        RunnerChoice::Embedded(runner) => {
            let mut session = ReplSession::with_runner(runner);
            if let Some(ref path) = script_path {
                if let Err(error) = run_script_file(&mut session, path, silent, &script_args) {
                    eprintln!("error: {error}");
                    if !interactive {
                        return Err(error);
                    }
                }
            }
            if should_repl {
                run_repl(session)?;
            }
        }
        RunnerChoice::Remote { runner, session: session_req } => {
            let mut session = ReplSession::with_session_request(runner, session_req);
            if let Some(ref path) = script_path {
                if let Err(error) = run_script_file(&mut session, path, silent, &script_args) {
                    eprintln!("error: {error}");
                    if !interactive {
                        return Err(error);
                    }
                }
            }
            if should_repl {
                run_repl(session)?;
            }
        }
    }

    Ok(())
}

fn run_repl<R: RuntimeRunner>(mut session: ReplSession<R>) -> Result<(), Box<dyn Error>> {
    let config = Config::builder().build();
    let completion_ui = CompletionUi::default();
    let mut editor = Editor::<ReplHelper, DefaultHistory>::with_config(config)?;
    editor.set_helper(Some(ReplHelper::new(completion_ui.clone())));
    editor.bind_sequence(
        KeyEvent(KeyCode::Enter, Modifiers::NONE),
        EventHandler::Conditional(Box::new(EnterEventHandler)),
    );
    editor.bind_sequence(
        KeyEvent::from('\t'),
        EventHandler::Conditional(Box::new(TabEventHandler {
            completion_ui: completion_ui.clone(),
        })),
    );
    editor.bind_sequence(
        KeyEvent(KeyCode::Esc, Modifiers::NONE),
        EventHandler::Conditional(Box::new(EscapeEventHandler {
            completion_ui: completion_ui.clone(),
        })),
    );
    editor.bind_sequence(
        KeyEvent::from('}'),
        EventHandler::Conditional(Box::new(RightBraceEventHandler)),
    );
    editor.bind_sequence(
        KeyEvent(KeyCode::Left, Modifiers::NONE),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::move_left())),
    );
    editor.bind_sequence(
        KeyEvent::ctrl('B'),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::move_left())),
    );
    editor.bind_sequence(
        KeyEvent(KeyCode::Home, Modifiers::NONE),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::move_to_boundary())),
    );
    editor.bind_sequence(
        KeyEvent::ctrl('A'),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::move_to_boundary())),
    );
    editor.bind_sequence(
        KeyEvent(KeyCode::Backspace, Modifiers::NONE),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::backspace())),
    );
    editor.bind_sequence(
        KeyEvent::ctrl('H'),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::backspace())),
    );
    editor.bind_sequence(
        KeyEvent(KeyCode::Delete, Modifiers::NONE),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::delete_forward())),
    );
    editor.bind_sequence(
        KeyEvent::ctrl('U'),
        EventHandler::Conditional(Box::new(ProtectedPrefixHandler::kill_to_boundary())),
    );
    loop {
        if let Some(helper) = editor.helper_mut() {
            helper.set_snapshot(session.completion_snapshot());
        }

        match editor.readline(PRIMARY_PROMPT) {
            Ok(line) => {
                editor.add_history_entry(line.as_str())?;
                let line = strip_continuation_prefixes(&line);

                match session.handle_line(&line) {
                    ReplOutput::Message(message) | ReplOutput::Error(message) => {
                        if !message.is_empty() {
                            println!("{message}");
                        }
                    }
                    ReplOutput::Exit => break,
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(error) => return Err(Box::new(error)),
        }
    }

    Ok(())
}

struct CliArgs {
    connect: Option<RemoteConnectSpec>,
    script_path: Option<String>,
    interactive: bool,
    silent: bool,
    script_args: Vec<RuntimeValue>,
}

fn run_script_file<R: RuntimeRunner>(
    session: &mut ReplSession<R>,
    path: &str,
    silent: bool,
    args: &[RuntimeValue],
) -> Result<(), Box<dyn Error>> {
    let text = fs::read_to_string(path)?;
    match session.run_script_text(path, &text, args) {
        Ok(value) => {
            if !silent && !is_script_noop_result(&value) {
                eprintln!("{}", session.render_value(&value));
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn is_script_noop_result(value: &RuntimeValue) -> bool {
    matches!(value, RuntimeValue::Inline(InlineValue::Tuple(values)) if values.is_empty())
}

enum RunnerChoice {
    Embedded(EmbeddedRunner),
    Remote {
        runner: RemoteRunner,
        session: SessionOpenRequest,
    },
}

struct RemoteConnectSpec {
    addr: String,
    session: SessionOpenRequest,
}

fn parse_cli_args() -> Result<CliArgs, Box<dyn Error>> {
    let all_args: Vec<String> = env::args().skip(1).collect();
    let sep = all_args.iter().position(|arg| arg == "--");
    let vox_args = match sep {
        Some(index) => &all_args[..index],
        None => all_args.as_slice(),
    };
    let raw_script_args: &[String] = match sep {
        Some(index) => &all_args[index + 1..],
        None => &[],
    };

    let mut args = vox_args.iter();
    let mut connect = None;
    let mut create_if_missing = false;
    let mut interactive = false;
    let mut silent = false;
    let mut script_path = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--connect" => {
                let Some(addr) = args.next() else {
                    return Err("`--connect` requires an address".into());
                };
                connect = Some(addr.clone());
            }
            "--new" => create_if_missing = true,
            "--interactive" | "-i" => interactive = true,
            "--silent" | "-s" => silent = true,
            "--help" | "-h" => {
                println!("Usage: vox-repl [OPTIONS] [SCRIPT] [-- SCRIPT_ARGS...]");
                println!();
                println!("Options:");
                println!("  -i, --interactive    Drop into REPL after running the script");
                println!("  -s, --silent         Suppress stderr output of trailing expressions");
                println!("  --connect ADDR       Connect to a remote vox-runtime instance");
                println!("  --new                Create session if missing (requires --connect)");
                println!("  -h, --help           Show this help message");
                println!();
                println!("Arguments after `--` are passed to the script as positional parameters:");
                println!("  integers → Int,  floats → Float,  true/false → Bool,  null → Null");
                println!("  everything else → String");
                std::process::exit(0);
            }
            other => {
                if other.starts_with('-') {
                    return Err(format!("unrecognized argument `{other}`").into());
                }
                if script_path.is_some() {
                    return Err("only one script file may be provided".into());
                }
                script_path = Some(other.to_owned());
            }
        }
    }

    let connect = match connect {
        Some(connect) => {
            if create_if_missing {
                let (_addr, selector) = split_connect_target(&connect)?;
                match selector {
                    Some(SessionSelector::Id(id)) => {
                        return Err(format!(
                            "`--new` cannot create a session with explicit id {}",
                            id.0
                        )
                        .into());
                    }
                    _ => {}
                }
            }
            Some(parse_remote_connect_spec(&connect, create_if_missing)?)
        }
        None => {
            if create_if_missing {
                return Err("`--new` requires `--connect host:port@session`".into());
            }
            None
        }
    };

    let script_args = raw_script_args
        .iter()
        .map(|arg| parse_script_arg(arg))
        .collect();

    Ok(CliArgs {
        connect,
        script_path,
        interactive,
        silent,
        script_args,
    })
}

fn parse_script_arg(raw: &str) -> RuntimeValue {
    if raw == "true" {
        RuntimeValue::Inline(InlineValue::Bool(true))
    } else if raw == "false" {
        RuntimeValue::Inline(InlineValue::Bool(false))
    } else if raw == "null" {
        RuntimeValue::Inline(InlineValue::Null)
    } else if let Ok(value) = raw.parse::<i64>() {
        RuntimeValue::Inline(InlineValue::Int(value))
    } else if let Ok(value) = raw.parse::<f64>() {
        RuntimeValue::Inline(InlineValue::Float(value))
    } else {
        RuntimeValue::Inline(InlineValue::String(raw.to_owned()))
    }
}

fn parse_remote_connect_spec(
    raw: &str,
    create_if_missing: bool,
) -> Result<RemoteConnectSpec, Box<dyn Error>> {
    let (addr, selector) = split_connect_target(raw)?;
    let session = match selector {
        Some(SessionSelector::Id(id)) => {
            if create_if_missing {
                return Err(
                    format!("`--new` cannot create a session with explicit id {}", id.0).into(),
                );
            }
            SessionOpenRequest {
                selector: Some(SessionSelector::Id(id)),
                mode: SessionOpenMode::Attach,
            }
        }
        Some(SessionSelector::Name(name)) => SessionOpenRequest {
            selector: Some(SessionSelector::Name(name)),
            mode: if create_if_missing {
                SessionOpenMode::AttachOrCreate
            } else {
                SessionOpenMode::Attach
            },
        },
        None => SessionOpenRequest {
            selector: None,
            mode: SessionOpenMode::Create,
        },
    };
    Ok(RemoteConnectSpec { addr, session })
}

fn split_connect_target(raw: &str) -> Result<(String, Option<SessionSelector>), Box<dyn Error>> {
    let (addr, target) = match raw.rsplit_once('@') {
        Some((_, target)) if target.trim().is_empty() => {
            return Err("session id or name after `@` must not be empty".into());
        }
        Some((addr, target)) => (addr.to_owned(), Some(target)),
        None => (raw.to_owned(), None),
    };
    if addr.trim().is_empty() {
        return Err("`--connect` requires an address".into());
    }

    let selector = match target {
        Some(target) => Some(parse_session_selector(target)?),
        None => None,
    };
    Ok((addr, selector))
}

fn parse_session_selector(raw: &str) -> Result<SessionSelector, Box<dyn Error>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("session id or name must not be empty".into());
    }
    match trimmed.parse::<u64>() {
        Ok(id) => Ok(SessionSelector::Id(vox_core::ids::SessionId(id))),
        Err(_) => Ok(SessionSelector::Name(trimmed.to_owned())),
    }
}

#[derive(Clone, Copy)]
struct EnterEventHandler;

impl ConditionalEventHandler for EnterEventHandler {
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
        let source = strip_continuation_prefixes(ctx.line());
        if !has_unclosed_braces(&source) {
            return None;
        }

        let prefix = strip_continuation_prefixes(&ctx.line()[..ctx.pos()]);
        let indentation = " ".repeat(indentation_for_continuation(&prefix));
        Some(Cmd::Insert(
            1,
            format!("\n{CONTINUATION_PROMPT}{indentation}"),
        ))
    }
}

#[derive(Clone)]
struct TabEventHandler {
    completion_ui: CompletionUi,
}

impl ConditionalEventHandler for TabEventHandler {
    fn handle(&self, _: &Event, n: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
        if editable_line_before_cursor(ctx)
            .chars()
            .all(char::is_whitespace)
        {
            return Some(Cmd::Insert(n, INDENT.to_owned()));
        }

        match self.completion_ui.prepare_tab(ctx.line(), ctx.pos()).ok()? {
            TabCompletion::UseDefault => None,
            TabCompletion::Noop => Some(Cmd::Noop),
            TabCompletion::Repaint => Some(Cmd::Repaint),
            TabCompletion::Insert(text) => Some(Cmd::Insert(1, text)),
            TabCompletion::Replace {
                delete,
                replacement,
            } => Some(Cmd::Replace(
                Movement::BackwardChar(delete),
                Some(replacement),
            )),
        }
    }
}

#[derive(Clone)]
struct EscapeEventHandler {
    completion_ui: CompletionUi,
}

impl ConditionalEventHandler for EscapeEventHandler {
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, _: &EventContext) -> Option<Cmd> {
        if self.completion_ui.dismiss_menu() {
            Some(Cmd::Repaint)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
struct RightBraceEventHandler;

impl ConditionalEventHandler for RightBraceEventHandler {
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
        let source = strip_continuation_prefixes(ctx.line());
        let current_line = editable_line_before_cursor(ctx);
        if !current_line.chars().all(char::is_whitespace) || !has_unclosed_braces(&source) {
            return None;
        }

        let existing_indent = current_line.chars().count();
        let prefix = strip_continuation_prefixes(&ctx.line()[..ctx.pos()]);
        let desired_indent = brace_depth(&prefix)
            .saturating_sub(1)
            .saturating_mul(INDENT.len());
        if existing_indent == desired_indent {
            return None;
        }

        let delete = RepeatCount::try_from(existing_indent).ok()?;
        Some(Cmd::Replace(
            Movement::BackwardChar(delete),
            Some(format!("{}{}", " ".repeat(desired_indent), '}')),
        ))
    }
}

#[derive(Clone, Copy)]
struct ProtectedPrefixHandler {
    behavior: ProtectedPrefixBehavior,
}

#[derive(Clone, Copy)]
enum ProtectedPrefixBehavior {
    MoveLeft,
    MoveToBoundary,
    Backspace,
    DeleteForward,
    KillToBoundary,
}

impl ProtectedPrefixHandler {
    fn move_left() -> Self {
        Self {
            behavior: ProtectedPrefixBehavior::MoveLeft,
        }
    }

    fn move_to_boundary() -> Self {
        Self {
            behavior: ProtectedPrefixBehavior::MoveToBoundary,
        }
    }

    fn backspace() -> Self {
        Self {
            behavior: ProtectedPrefixBehavior::Backspace,
        }
    }

    fn delete_forward() -> Self {
        Self {
            behavior: ProtectedPrefixBehavior::DeleteForward,
        }
    }

    fn kill_to_boundary() -> Self {
        Self {
            behavior: ProtectedPrefixBehavior::KillToBoundary,
        }
    }
}

impl ConditionalEventHandler for ProtectedPrefixHandler {
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
        let boundary = current_line_protected_boundary(ctx)?;
        let pos = ctx.pos();

        match self.behavior {
            ProtectedPrefixBehavior::MoveLeft => clamp_leftward_motion(pos, boundary),
            ProtectedPrefixBehavior::MoveToBoundary => move_to_boundary(pos, boundary),
            ProtectedPrefixBehavior::Backspace => clamp_leftward_motion(pos, boundary),
            ProtectedPrefixBehavior::DeleteForward => {
                if pos < boundary {
                    move_to_boundary(pos, boundary)
                } else {
                    None
                }
            }
            ProtectedPrefixBehavior::KillToBoundary => {
                if pos < boundary {
                    move_to_boundary(pos, boundary)
                } else if pos == boundary {
                    Some(Cmd::Noop)
                } else {
                    let count = RepeatCount::try_from(pos - boundary).ok()?;
                    Some(Cmd::Kill(Movement::BackwardChar(count)))
                }
            }
        }
    }
}

fn current_line_before_cursor<'a>(ctx: &'a EventContext<'_>) -> &'a str {
    ctx.line()[..ctx.pos()].rsplit('\n').next().unwrap_or("")
}

fn editable_line_before_cursor<'a>(ctx: &'a EventContext<'_>) -> &'a str {
    strip_current_line_prefix(current_line_before_cursor(ctx))
}

fn indentation_for_continuation(prefix: &str) -> usize {
    brace_depth(prefix).saturating_mul(INDENT.len())
}

fn has_unclosed_braces(source: &str) -> bool {
    brace_depth(source) > 0
}

fn brace_depth(source: &str) -> usize {
    let mut depth = 0usize;
    let mut chars = source.chars();
    let mut in_string = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' => escape = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    depth
}

fn strip_continuation_prefixes(source: &str) -> String {
    source
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                line.to_owned()
            } else {
                strip_current_line_prefix(line).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_current_line_prefix(line: &str) -> &str {
    line.strip_prefix(CONTINUATION_PROMPT).unwrap_or(line)
}

fn current_line_protected_boundary(ctx: &EventContext<'_>) -> Option<usize> {
    let line_start = ctx.line()[..ctx.pos()]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    ctx.line()[line_start..]
        .starts_with(CONTINUATION_PROMPT)
        .then_some(line_start + CONTINUATION_PROMPT.len())
}

fn clamp_leftward_motion(pos: usize, boundary: usize) -> Option<Cmd> {
    if pos < boundary {
        move_to_boundary(pos, boundary)
    } else if pos == boundary {
        Some(Cmd::Noop)
    } else {
        None
    }
}

fn move_to_boundary(pos: usize, boundary: usize) -> Option<Cmd> {
    match pos.cmp(&boundary) {
        std::cmp::Ordering::Less => {
            let count = RepeatCount::try_from(boundary - pos).ok()?;
            Some(Cmd::Move(Movement::ForwardChar(count)))
        }
        std::cmp::Ordering::Equal => Some(Cmd::Noop),
        std::cmp::Ordering::Greater => {
            let count = RepeatCount::try_from(pos - boundary).ok()?;
            Some(Cmd::Move(Movement::BackwardChar(count)))
        }
    }
}
