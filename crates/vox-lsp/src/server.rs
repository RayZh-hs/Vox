use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use vox_compiler::frontend;
use vox_core::diagnostics::DiagnosticBag;
use vox_core::source::SourceText;

pub struct VoxLanguageServer {
    client: Client,
}

impl VoxLanguageServer {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

fn collect_diagnostics(path: &str, source: &str) -> Option<Vec<Diagnostic>> {
    let source_text = SourceText::new(path, 0, source);
    let result = frontend::analyze_source(&source_text);

    match result {
        Ok(_) => Some(Vec::new()),
        Err(bag) => Some(convert_diagnostics(&source_text.text, bag)),
    }
}

fn convert_diagnostics(source: &str, bag: DiagnosticBag) -> Vec<Diagnostic> {
    bag.into_vec()
        .into_iter()
        .map(|diag| convert_diagnostic(source, diag))
        .collect()
}

fn convert_diagnostic(source: &str, diag: vox_core::diagnostics::Diagnostic) -> Diagnostic {
    let severity = match diag.severity {
        vox_core::diagnostics::Severity::Note => DiagnosticSeverity::INFORMATION,
        vox_core::diagnostics::Severity::Warning => DiagnosticSeverity::WARNING,
        vox_core::diagnostics::Severity::Error => DiagnosticSeverity::ERROR,
    };

    let range = diag
        .span
        .map(|span| byte_span_to_range(source, span.start, span.end))
        .unwrap_or_default();

    Diagnostic {
        range,
        severity: Some(severity),
        source: Some("vox".to_string()),
        message: diag.message,
        ..Default::default()
    }
}

fn byte_span_to_range(source: &str, start: usize, end: usize) -> Range {
    let start = byte_offset_to_position(source, start);
    let end = byte_offset_to_position(source, end);
    Range { start, end }
}

fn byte_offset_to_position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];
    let line = prefix.chars().filter(|&c| c == '\n').count() as u32;
    let last_newline = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = prefix[last_newline..].chars().count() as u32;
    Position { line, character }
}

#[tower_lsp::async_trait]
impl LanguageServer for VoxLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let diagnostics =
            collect_diagnostics(params.text_document.uri.as_str(), &params.text_document.text)
                .unwrap_or_default();
        self.client
            .publish_diagnostics(params.text_document.uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            let diagnostics =
                collect_diagnostics(params.text_document.uri.as_str(), &change.text)
                    .unwrap_or_default();
            self.client
                .publish_diagnostics(params.text_document.uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }
}
