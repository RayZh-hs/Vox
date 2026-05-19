use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Note,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    pub start: usize,
    pub end: usize,
}

impl TextSpan {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Option<TextSpan>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            span: None,
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            span: None,
        }
    }

    pub fn with_span(mut self, span: TextSpan) -> Self {
        self.span = Some(span);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticBag {
    entries: Vec<Diagnostic>,
}

impl DiagnosticBag {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.entries.push(diagnostic);
    }

    pub fn extend(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        self.entries.extend(diagnostics);
    }

    pub fn has_errors(&self) -> bool {
        self.entries
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.entries.iter()
    }

    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.entries
    }
}

impl From<Vec<Diagnostic>> for DiagnosticBag {
    fn from(entries: Vec<Diagnostic>) -> Self {
        Self { entries }
    }
}

impl fmt::Display for DiagnosticBag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for diagnostic in &self.entries {
            writeln!(f, "{:?}: {}", diagnostic.severity, diagnostic.message)?;
        }

        Ok(())
    }
}
