use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};

use rustyline::{
    Context, Helper, RepeatCount, Result as RustylineResult,
    completion::{Candidate, Completer, FilenameCompleter, Pair, longest_common_prefix},
    highlight::{CmdKind, Highlighter, MatchingBracketHighlighter},
    hint::{Hint, Hinter},
    history::DefaultHistory,
    validate::{MatchingBracketValidator, ValidationContext, ValidationResult, Validator},
};

const MENU_WRAP_COLUMNS: usize = 80;
const MENU_COLUMN_PADDING: usize = 2;
const MENU_HINT_STYLE: &str = "\x1b[2m";
const MENU_HINT_RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Default)]
pub struct CompletionSnapshot {
    pub commands: Vec<String>,
    pub snapshots: Vec<String>,
    pub xopts: Vec<String>,
    pub handles: Vec<String>,
    pub sessions: Vec<String>,
    pub session_commands: Vec<String>,
    pub symbols: Vec<String>,
}

impl CompletionSnapshot {
    pub fn complete_symbol(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .symbols
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }

    fn complete_commands(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .commands
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }

    fn complete_xopts(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .xopts
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }

    fn complete_snapshots(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .snapshots
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }

    fn complete_handles(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .handles
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }

    fn complete_sessions(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .sessions
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }

    fn complete_session_commands(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .session_commands
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompletionUi {
    state: Arc<Mutex<CompletionUiState>>,
}

#[derive(Debug, Default)]
struct CompletionUiState {
    snapshot: CompletionSnapshot,
    menu: Option<CompletionMenu>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletionMenu {
    line: String,
    pos: usize,
    hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionHint(String);

impl Hint for CompletionHint {
    fn display(&self) -> &str {
        &self.0
    }

    fn completion(&self) -> Option<&str> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabCompletion {
    UseDefault,
    Noop,
    Repaint,
    Insert(String),
    Replace {
        delete: RepeatCount,
        replacement: String,
    },
}

impl CompletionUi {
    pub fn set_snapshot(&self, snapshot: CompletionSnapshot) {
        let mut state = self.state.lock().unwrap();
        state.snapshot = snapshot;
        state.menu = None;
    }

    pub fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> RustylineResult<(usize, Vec<Pair>)> {
        let snapshot = self.state.lock().unwrap().snapshot.clone();
        complete_line(&snapshot, line, pos, ctx)
    }

    pub fn prepare_tab(&self, line: &str, pos: usize) -> RustylineResult<TabCompletion> {
        self.clear_menu();

        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let (start, candidates) = self.complete(line, pos, &ctx)?;
        if candidates.is_empty() {
            return Ok(TabCompletion::UseDefault);
        }

        if candidates.len() == 1 {
            return Ok(replacement_command(
                line,
                start,
                pos,
                candidates[0].replacement(),
            ));
        }

        let replacement = longest_common_prefix(&candidates)
            .filter(|prefix| prefix.len() > pos.saturating_sub(start))
            .map(str::to_owned);
        let hint = render_menu_hint(&candidates);

        if let Some(replacement) = replacement {
            let preview = replace_range(line, start, pos, &replacement);
            self.store_menu(preview, start + replacement.len(), hint);
            Ok(replacement_command(line, start, pos, &replacement))
        } else {
            self.store_menu(line.to_owned(), pos, hint);
            Ok(TabCompletion::Repaint)
        }
    }

    fn hint_for(&self, _: &str, _: usize) -> Option<CompletionHint> {
        self.state
            .lock()
            .unwrap()
            .menu
            .as_ref()
            .map(|menu| CompletionHint(menu.hint.clone()))
    }

    fn handle_highlight_event(&self, kind: CmdKind) -> bool {
        if !matches!(kind, CmdKind::ForcedRefresh) {
            return false;
        }

        let mut state = self.state.lock().unwrap();
        if state.menu.is_some() {
            state.menu = None;
            true
        } else {
            false
        }
    }

    pub fn clear_menu(&self) {
        let mut state = self.state.lock().unwrap();
        state.menu = None;
    }

    pub fn dismiss_menu(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.menu.take().is_some()
    }

    fn store_menu(&self, line: String, pos: usize, hint: String) {
        let mut state = self.state.lock().unwrap();
        state.menu = Some(CompletionMenu { line, pos, hint });
    }
}

pub struct ReplHelper {
    completion_ui: CompletionUi,
    highlighter: MatchingBracketHighlighter,
    validator: MatchingBracketValidator,
}

impl ReplHelper {
    pub fn new(completion_ui: CompletionUi) -> Self {
        Self {
            completion_ui,
            highlighter: MatchingBracketHighlighter::default(),
            validator: MatchingBracketValidator::default(),
        }
    }

    pub fn set_snapshot(&mut self, snapshot: CompletionSnapshot) {
        self.completion_ui.set_snapshot(snapshot);
    }
}

impl Default for ReplHelper {
    fn default() -> Self {
        Self::new(CompletionUi::default())
    }
}

impl Helper for ReplHelper {}

impl Validator for ReplHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> RustylineResult<ValidationResult> {
        if ctx.input().trim_start().starts_with(':') {
            return Ok(ValidationResult::Valid(None));
        }
        self.validator.validate(ctx)
    }
}

impl Highlighter for ReplHelper {
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
        self.highlighter.highlight(line, pos)
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        if hint.starts_with('\n') {
            Cow::Owned(format!("{MENU_HINT_STYLE}{hint}{MENU_HINT_RESET}"))
        } else {
            Cow::Borrowed(hint)
        }
    }

    fn highlight_char(&self, line: &str, pos: usize, kind: CmdKind) -> bool {
        self.completion_ui.handle_highlight_event(kind)
            || self.highlighter.highlight_char(line, pos, kind)
    }
}

impl Hinter for ReplHelper {
    type Hint = CompletionHint;

    fn hint(&self, line: &str, pos: usize, _: &Context<'_>) -> Option<Self::Hint> {
        self.completion_ui.hint_for(line, pos)
    }
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> RustylineResult<(usize, Vec<Pair>)> {
        self.completion_ui.complete(line, pos, ctx)
    }
}

fn symbol_start(prefix: &str) -> usize {
    prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_symbol_char(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0)
}

fn command_token_start(prefix: &str, minimum: usize) -> usize {
    symbol_start(prefix).max(minimum)
}

fn is_symbol_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.')
}

fn to_pairs(candidates: Vec<String>) -> Vec<Pair> {
    candidates
        .into_iter()
        .map(|candidate| Pair {
            display: candidate.clone(),
            replacement: candidate,
        })
        .collect()
}

fn complete_line(
    snapshot: &CompletionSnapshot,
    line: &str,
    pos: usize,
    ctx: &Context<'_>,
) -> RustylineResult<(usize, Vec<Pair>)> {
    let prefix = &line[..pos];
    if prefix.starts_with(':') {
        return complete_command_line(snapshot, line, pos, ctx);
    }

    let start = symbol_start(prefix);
    let token = &prefix[start..];
    Ok((start, snapshot.complete_symbol(token)))
}

fn complete_command_line(
    snapshot: &CompletionSnapshot,
    line: &str,
    pos: usize,
    ctx: &Context<'_>,
) -> RustylineResult<(usize, Vec<Pair>)> {
    let prefix = &line[..pos];
    let Some(space) = prefix.find(char::is_whitespace) else {
        return Ok((0, snapshot.complete_commands(prefix)));
    };

    let command = prefix[..space].trim();
    let argument_start = space
        + prefix[space..]
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .count();

    match command {
        ":snapshot" | ":restore" => {
            let start = command_token_start(prefix, argument_start);
            Ok((start, snapshot.complete_snapshots(&prefix[start..])))
        }
        ":run" => FilenameCompleter::new().complete(line, pos, ctx),
        ":xopt" => {
            let start = command_token_start(prefix, argument_start);
            Ok((start, snapshot.complete_xopts(&prefix[start..])))
        }
        ":show" => {
            let start = command_token_start(prefix, argument_start);
            Ok((start, snapshot.complete_handles(&prefix[start..])))
        }
        ":drop" | ":type" => {
            let start = command_token_start(prefix, argument_start);
            Ok((start, snapshot.complete_symbol(&prefix[start..])))
        }
        ":session" => complete_session_line(snapshot, prefix, argument_start),
        _ => Ok((0, Vec::new())),
    }
}

fn complete_session_line(
    snapshot: &CompletionSnapshot,
    prefix: &str,
    argument_start: usize,
) -> RustylineResult<(usize, Vec<Pair>)> {
    let rest = &prefix[argument_start..];
    let trimmed = rest.trim_start();
    if trimmed.is_empty() {
        return Ok((argument_start, snapshot.complete_session_commands("")));
    }

    let leading_ws = rest.len() - trimmed.len();
    let subcommand_start = argument_start + leading_ws;
    let Some(space) = trimmed.find(char::is_whitespace) else {
        return Ok((
            subcommand_start,
            snapshot.complete_session_commands(trimmed),
        ));
    };

    let subcommand = trimmed[..space].trim();
    let args_start = subcommand_start
        + space
        + trimmed[space..]
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .count();
    match subcommand {
        "connect" | "new" => {
            let start = command_token_start(prefix, args_start);
            Ok((start, snapshot.complete_sessions(&prefix[start..])))
        }
        _ => Ok((0, Vec::new())),
    }
}

fn replacement_command(line: &str, start: usize, pos: usize, replacement: &str) -> TabCompletion {
    let current = &line[start..pos];
    if current == replacement {
        TabCompletion::Noop
    } else if let Some(suffix) = replacement.strip_prefix(current) {
        TabCompletion::Insert(suffix.to_owned())
    } else {
        TabCompletion::Replace {
            delete: current
                .chars()
                .count()
                .try_into()
                .unwrap_or(RepeatCount::MAX),
            replacement: replacement.to_owned(),
        }
    }
}

fn replace_range(line: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut updated = String::with_capacity(line.len() + replacement.len());
    updated.push_str(&line[..start]);
    updated.push_str(replacement);
    updated.push_str(&line[end..]);
    updated
}

fn render_menu_hint(candidates: &[Pair]) -> String {
    let column_width = candidates
        .iter()
        .map(|candidate| candidate.display.chars().count())
        .max()
        .unwrap_or(0)
        + MENU_COLUMN_PADDING;

    let mut hint = String::from("\n");
    let mut current_width = 0usize;

    for candidate in candidates {
        let text = candidate.display();
        let text_width = text.chars().count();
        let cell_width = column_width.max(text_width);

        if current_width != 0 && current_width + cell_width > MENU_WRAP_COLUMNS {
            hint.push('\n');
            current_width = 0;
        }

        hint.push_str(text);
        current_width += text_width;

        let remaining_padding = cell_width.saturating_sub(text_width);
        if remaining_padding != 0 {
            hint.extend(std::iter::repeat_n(' ', remaining_padding));
            current_width += remaining_padding;
        }
    }

    while hint.ends_with(' ') {
        hint.pop();
    }

    hint
}
