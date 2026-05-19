use camino::Utf8PathBuf;

use crate::diagnostics::{Diagnostic, TextSpan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Package,
    Script { evil: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModulePath {
    segments: Vec<String>,
}

impl ModulePath {
    pub fn parse(raw: &str) -> Result<Self, Diagnostic> {
        let segments = raw
            .split('.')
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();

        if segments.is_empty() {
            return Err(Diagnostic::error("module path cannot be empty"));
        }

        if let Some(invalid) = segments.iter().find(|segment| !is_identifier(segment)) {
            return Err(Diagnostic::error(format!(
                "invalid module segment `{invalid}`"
            )));
        }

        Ok(Self { segments })
    }

    pub fn as_str(&self) -> String {
        self.segments.join(".")
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOrigin {
    pub path: Utf8PathBuf,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceText {
    pub origin: SourceOrigin,
    pub text: String,
}

impl SourceText {
    pub fn new(path: impl Into<Utf8PathBuf>, revision: u64, text: impl Into<String>) -> Self {
        Self {
            origin: SourceOrigin {
                path: path.into(),
                revision,
            },
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceHeader {
    pub kind: ModuleKind,
    pub module: ModulePath,
    pub span: TextSpan,
}

pub fn is_identifier(raw: &str) -> bool {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }

    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
