use std::collections::HashMap;
use std::sync::Mutex;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use vox_compiler::frontend;
use vox_core::diagnostics::DiagnosticBag;
use vox_core::source::SourceText;

pub struct VoxLanguageServer {
    client: Client,
    documents: Mutex<HashMap<Url, String>>,
}

impl VoxLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
        }
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

const TOKEN_KEYWORD: u32 = 0;
const TOKEN_VARIABLE: u32 = 1;
const TOKEN_STRING: u32 = 2;
const TOKEN_NUMBER: u32 = 3;
const TOKEN_COMMENT: u32 = 4;
const TOKEN_OPERATOR: u32 = 5;

fn compute_semantic_tokens(source: &str) -> Vec<SemanticToken> {
    use vox_compiler::frontend::lexer::{Lexer, TokenKind};

    let Ok(tokens) = Lexer::new(source, 0).lex() else {
        return Vec::new();
    };

    let mut result = Vec::new();
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;

    for token in &tokens {
        let token_type = match &token.kind {
            TokenKind::DocComment(_) => TOKEN_COMMENT,
            TokenKind::Identifier(_) => TOKEN_VARIABLE,
            TokenKind::Integer(_) | TokenKind::Float(_) => TOKEN_NUMBER,
            TokenKind::StringLiteral(_) => TOKEN_STRING,
            TokenKind::As
            | TokenKind::Dyn
            | TokenKind::Econ
            | TokenKind::Else
            | TokenKind::Evil
            | TokenKind::False
            | TokenKind::For
            | TokenKind::Fun
            | TokenKind::If
            | TokenKind::Import
            | TokenKind::In
            | TokenKind::Is
            | TokenKind::Null
            | TokenKind::Package
            | TokenKind::Panic
            | TokenKind::Param
            | TokenKind::Private
            | TokenKind::Public
            | TokenKind::Return
            | TokenKind::Script
            | TokenKind::True
            | TokenKind::Val
            | TokenKind::Var
            | TokenKind::When => TOKEN_KEYWORD,
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Bang
            | TokenKind::Assign
            | TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq
            | TokenKind::EqEq
            | TokenKind::BangEq
            | TokenKind::Less
            | TokenKind::LessEq
            | TokenKind::Greater
            | TokenKind::GreaterEq
            | TokenKind::AmpAmp
            | TokenKind::PipePipe
            | TokenKind::QuestionDot
            | TokenKind::QuestionColon
            | TokenKind::BangBang
            | TokenKind::DotDot
            | TokenKind::DotDotEq
            | TokenKind::Arrow
            | TokenKind::FatArrow
            | TokenKind::Question
            | TokenKind::Dot => TOKEN_OPERATOR,
            TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::Comma
            | TokenKind::Hash
            | TokenKind::Colon
            | TokenKind::Semicolon
            | TokenKind::Eof => continue,
        };

        let start_pos = byte_offset_to_position(source, token.span.start);
        let end_pos = byte_offset_to_position(source, token.span.end);

        let delta_line = start_pos.line - prev_line;
        let delta_start = if delta_line == 0 {
            start_pos.character - prev_start
        } else {
            start_pos.character
        };
        let length = end_pos.character - start_pos.character;

        result.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });

        prev_line = start_pos.line;
        prev_start = start_pos.character;
    }

    result
}

#[allow(deprecated)]
fn compute_document_symbols(source: &str) -> Vec<DocumentSymbol> {
    use vox_compiler::frontend::ast::TopLevelItem;

    let source_text = SourceText::new("", 0, source);
    let Ok(unit) = frontend::analyze_source(&source_text) else {
        return Vec::new();
    };

    let mut symbols = Vec::new();

    for item in &unit.syntax.items {
        let (name, kind, span) = match item {
            TopLevelItem::Function(f) => (f.name.clone(), SymbolKind::FUNCTION, f.span.clone()),
            TopLevelItem::Value(v) => (v.name.clone(), SymbolKind::VARIABLE, v.span.clone()),
            TopLevelItem::Param(p) => (p.name.clone(), SymbolKind::VARIABLE, p.span.clone()),
            TopLevelItem::Import(i) => (i.module.to_source_string(), SymbolKind::MODULE, i.span.clone()),
            TopLevelItem::Statement(_) => continue,
        };

        let range = byte_span_to_range(source, span.start, span.end);

        symbols.push(DocumentSymbol {
            name,
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range,
            selection_range: range,
            children: None,
        });
    }

    symbols
}

#[tower_lsp::async_trait]
impl LanguageServer for VoxLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                        work_done_progress_options: Default::default(),
                        legend: SemanticTokensLegend {
                            token_types: vec![
                                "keyword".into(),
                                "variable".into(),
                                "string".into(),
                                "number".into(),
                                "comment".into(),
                                "operator".into(),
                            ],
                            token_modifiers: vec![],
                        },
                        range: Some(false),
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                    }),
                ),
                document_symbol_provider: Some(OneOf::Right(DocumentSymbolOptions {
                    work_done_progress_options: Default::default(),
                    label: None,
                })),
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
        self.documents
            .lock()
            .unwrap()
            .insert(params.text_document.uri.clone(), params.text_document.text.clone());
        let diagnostics =
            collect_diagnostics(params.text_document.uri.as_str(), &params.text_document.text)
                .unwrap_or_default();
        self.client
            .publish_diagnostics(params.text_document.uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents
                .lock()
                .unwrap()
                .insert(params.text_document.uri.clone(), change.text.clone());
            let diagnostics =
                collect_diagnostics(params.text_document.uri.as_str(), &change.text)
                    .unwrap_or_default();
            self.client
                .publish_diagnostics(params.text_document.uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.lock().unwrap().remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document.uri)
            .cloned();

        match source {
            Some(text) => {
                let data = compute_semantic_tokens(&text);
                Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                    result_id: None,
                    data,
                })))
            }
            None => Ok(None),
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document.uri)
            .cloned();

        match source {
            Some(text) => {
                let symbols = compute_document_symbols(&text);
                Ok(Some(DocumentSymbolResponse::Nested(symbols)))
            }
            None => Ok(None),
        }
    }
}
