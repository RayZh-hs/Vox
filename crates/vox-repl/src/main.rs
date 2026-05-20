use rustyline::{
    Cmd, ConditionalEventHandler, Config, Editor, Event, EventContext, EventHandler, KeyCode,
    KeyEvent, Modifiers, Movement, RepeatCount, history::DefaultHistory,
};
use vox_repl::{ReplHelper, ReplOutput, ReplSession};

const PRIMARY_PROMPT: &str = ">>> ";
const CONTINUATION_PROMPT: &str = "... ";
const INDENT: &str = "    ";

fn main() -> rustyline::Result<()> {
    let config = Config::builder().build();
    let mut editor = Editor::<ReplHelper, DefaultHistory>::with_config(config)?;
    editor.set_helper(Some(ReplHelper::default()));
    editor.bind_sequence(
        KeyEvent(KeyCode::Enter, Modifiers::NONE),
        EventHandler::Conditional(Box::new(EnterEventHandler)),
    );
    editor.bind_sequence(
        KeyEvent::from('\t'),
        EventHandler::Conditional(Box::new(TabEventHandler)),
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
    let mut session = ReplSession::default();

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
            Err(error) => return Err(error),
        }
    }

    Ok(())
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

#[derive(Clone, Copy)]
struct TabEventHandler;

impl ConditionalEventHandler for TabEventHandler {
    fn handle(&self, _: &Event, n: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
        if editable_line_before_cursor(ctx)
            .chars()
            .all(char::is_whitespace)
        {
            Some(Cmd::Insert(n, INDENT.to_owned()))
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
