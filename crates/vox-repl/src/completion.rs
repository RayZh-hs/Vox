use rustyline::{
    Context, Helper, Result as RustylineResult,
    completion::{Completer, FilenameCompleter, Pair},
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
};

#[derive(Debug, Clone, Default)]
pub struct CompletionSnapshot {
    pub commands: Vec<String>,
    pub command_args: Vec<String>,
    pub xopts: Vec<String>,
    pub handles: Vec<String>,
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

    fn complete_command_args(&self, prefix: &str) -> Vec<Pair> {
        let mut candidates = self
            .command_args
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        to_pairs(candidates)
    }
}

#[derive(Default)]
pub struct ReplHelper {
    snapshot: CompletionSnapshot,
    files: FilenameCompleter,
}

impl ReplHelper {
    pub fn set_snapshot(&mut self, snapshot: CompletionSnapshot) {
        self.snapshot = snapshot;
    }
}

impl Helper for ReplHelper {}
impl Validator for ReplHelper {}
impl Highlighter for ReplHelper {}

impl Hinter for ReplHelper {
    type Hint = String;
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> RustylineResult<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        if prefix.starts_with(':') {
            return self.complete_command_line(line, pos, ctx);
        }

        let start = symbol_start(prefix);
        let token = &prefix[start..];
        Ok((start, self.snapshot.complete_symbol(token)))
    }
}

impl ReplHelper {
    fn complete_command_line(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> RustylineResult<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        let Some(space) = prefix.find(char::is_whitespace) else {
            return Ok((0, self.snapshot.complete_commands(prefix)));
        };

        let command = prefix[..space].trim();
        let argument_start = space
            + prefix[space..]
                .chars()
                .take_while(|ch| ch.is_whitespace())
                .count();

        match command {
            ":load" => self.files.complete(line, pos, ctx),
            ":xopt" => {
                let start = command_token_start(prefix, argument_start);
                Ok((start, self.snapshot.complete_xopts(&prefix[start..])))
            }
            ":show" => {
                let start = command_token_start(prefix, argument_start);
                Ok((start, self.snapshot.complete_handles(&prefix[start..])))
            }
            ":drop" | ":type" | ":purity" => {
                let start = command_token_start(prefix, argument_start);
                Ok((start, self.snapshot.complete_symbol(&prefix[start..])))
            }
            ":run" => {
                let start = command_token_start(prefix, argument_start);
                let token = &prefix[start..];
                let mut candidates = self.snapshot.complete_symbol(token);
                candidates.extend(self.snapshot.complete_command_args(token));
                candidates.sort_by(|left, right| left.replacement.cmp(&right.replacement));
                candidates.dedup_by(|left, right| left.replacement == right.replacement);
                Ok((start, candidates))
            }
            _ => Ok((0, Vec::new())),
        }
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
