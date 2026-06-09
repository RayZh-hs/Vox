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

fn position_to_byte_offset(source: &str, position: Position) -> usize {
    let mut current_line = 0u32;
    let mut current_char = 0u32;
    for (idx, c) in source.char_indices() {
        if current_line == position.line && current_char == position.character {
            return idx;
        }
        if c == '\n' {
            current_line += 1;
            current_char = 0;
        } else {
            current_char += 1;
        }
    }
    source.len()
}

fn segment_at_offset(_source: &str, qname: &vox_compiler::frontend::ast::QualifiedName, offset: usize) -> Option<String> {
    if offset < qname.span.start || offset >= qname.span.end {
        return None;
    }
    let rel = offset - qname.span.start;
    let mut pos = 0usize;
    for seg in &qname.segments {
        let seg_end = pos + seg.len();
        if rel >= pos && rel < seg_end {
            return Some(seg.clone());
        }
        pos = seg_end + 1;
    }
    None
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

fn build_symbol_table(unit: &vox_compiler::frontend::ast::FrontendUnit) -> HashMap<String, vox_core::diagnostics::TextSpan> {
    use vox_compiler::frontend::ast::*;
    let mut symbols = HashMap::new();

    fn walk_expr(expr: &Expr, symbols: &mut HashMap<String, vox_core::diagnostics::TextSpan>) {
        match &expr.kind {
            ExprKind::Block(block) => {
                walk_block_items(&block.items, symbols);
                if let Some(ref trailing) = block.trailing {
                    walk_expr(trailing, symbols);
                }
            }
            ExprKind::If(if_expr) => {
                for branch in &if_expr.branches {
                    walk_expr(&branch.condition, symbols);
                    walk_block_items(&branch.body.items, symbols);
                    if let Some(ref trailing) = branch.body.trailing {
                        walk_expr(trailing, symbols);
                    }
                }
                if let Some(ref else_branch) = if_expr.else_branch {
                    walk_block_items(&else_branch.items, symbols);
                    if let Some(ref trailing) = else_branch.trailing {
                        walk_expr(trailing, symbols);
                    }
                }
            }
            ExprKind::When(when_expr) => {
                walk_expr(&when_expr.subject, symbols);
                for arm in &when_expr.arms {
                    if let Some(ref binding) = arm.binding {
                        symbols.insert(binding.clone(), arm.span.clone());
                    }
                    walk_expr(&arm.body, symbols);
                }
                if let Some(ref else_arm) = when_expr.else_arm {
                    walk_expr(else_arm, symbols);
                }
            }
            ExprKind::For(for_expr) => {
                symbols.insert(for_expr.pattern.clone(), for_expr.span.clone());
                walk_expr(&for_expr.iterable, symbols);
                walk_block_items(&for_expr.body.items, symbols);
                if let Some(ref trailing) = for_expr.body.trailing {
                    walk_expr(trailing, symbols);
                }
            }
            ExprKind::Lambda(lambda) => {
                for param in &lambda.parameters {
                    symbols.insert(param.name.clone(), param.span.clone());
                }
                walk_expr(&lambda.body, symbols);
            }
            ExprKind::Call { callee, arguments } => {
                walk_expr(callee, symbols);
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) => walk_expr(e, symbols),
                        Argument::Named { value, .. } => walk_expr(value, symbols),
                    }
                }
            }
            ExprKind::ReceiverCall { receiver, arguments, .. } => {
                walk_expr(receiver, symbols);
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) => walk_expr(e, symbols),
                        Argument::Named { value, .. } => walk_expr(value, symbols),
                    }
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    walk_expr(item, symbols);
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    walk_expr(&field.value, symbols);
                }
            }
            ExprKind::Index { target, index } => {
                walk_expr(target, symbols);
                walk_expr(index, symbols);
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => {
                walk_expr(target, symbols);
            }
            ExprKind::Unary { expr: inner, .. } => walk_expr(inner, symbols),
            ExprKind::Binary { left, right, .. } => {
                walk_expr(left, symbols);
                walk_expr(right, symbols);
            }
            ExprKind::Range(range) => {
                if let Some(ref start) = range.start {
                    walk_expr(start, symbols);
                }
                if let Some(ref end) = range.end {
                    walk_expr(end, symbols);
                }
            }
            ExprKind::Intrinsic(intr) => match intr {
                IntrinsicExpr::Updated(u) => {
                    walk_expr(&u.target, symbols);
                    for upd in &u.updates {
                        walk_expr(&upd.value, symbols);
                    }
                }
                IntrinsicExpr::Econ(e) => {
                    walk_block_items(&e.body.items, symbols);
                    if let Some(ref trailing) = e.body.trailing {
                        walk_expr(trailing, symbols);
                    }
                }
            },
            ExprKind::Name(_)
            | ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::String(_) => {}
        }
    }

    fn walk_block_items(
        items: &[BlockItem],
        symbols: &mut HashMap<String, vox_core::diagnostics::TextSpan>,
    ) {
        for item in items {
            match item {
                BlockItem::LocalValue(lv) => {
                    symbols.insert(lv.name.clone(), lv.span.clone());
                    walk_expr(&lv.initializer, symbols);
                }
                BlockItem::Assignment(a) => {
                    walk_expr(&a.value, symbols);
                }
                BlockItem::CompoundAssignment(ca) => {
                    walk_expr(&ca.value, symbols);
                }
                BlockItem::Return(r) => {
                    if let Some(ref val) = r.value {
                        walk_expr(val, symbols);
                    }
                }
                BlockItem::Panic(_) => {}
                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => walk_expr(e, symbols),
            }
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                symbols.insert(f.name.clone(), f.span.clone());
                for param in &f.parameters {
                    symbols.insert(param.name.clone(), param.span.clone());
                }
                walk_expr(&f.body, &mut symbols);
            }
            TopLevelItem::Value(v) => {
                symbols.insert(v.name.clone(), v.span.clone());
                walk_expr(&v.initializer, &mut symbols);
            }
            TopLevelItem::Param(p) => {
                symbols.insert(p.name.clone(), p.span.clone());
            }
            TopLevelItem::Import(i) => {
                if let Some(last) = i.module.segments.last() {
                    symbols.insert(last.clone(), i.span.clone());
                }
            }
            TopLevelItem::Statement(s) => match s {
                BlockItem::LocalValue(lv) => {
                    symbols.insert(lv.name.clone(), lv.span.clone());
                    walk_expr(&lv.initializer, &mut symbols);
                }
                _ => {}
            },
        }
    }

    symbols
}

fn find_name_at_offset(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    source: &str,
    offset: usize,
) -> Option<String> {
    use vox_compiler::frontend::ast::*;

    fn find_in_expr(expr: &Expr, source: &str, offset: usize) -> Option<String> {
        match &expr.kind {
            ExprKind::Name(qname) => {
                return segment_at_offset(source, qname, offset);
            }
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => {
                if let Some(name) = segment_at_offset(source, callee, offset) {
                    return Some(name);
                }
                if let Some(name) = find_in_expr(receiver, source, offset) {
                    return Some(name);
                }
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            if let Some(name) = find_in_expr(e, source, offset) {
                                return Some(name);
                            }
                        }
                    }
                }
                return None;
            }
            _ => {}
        }

        match &expr.kind {
            ExprKind::Block(block) => {
                for item in &block.items {
                    if let Some(name) = find_in_block_item(item, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref trailing) = block.trailing {
                    if let Some(name) = find_in_expr(trailing, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::If(if_expr) => {
                for branch in &if_expr.branches {
                    if let Some(name) = find_in_expr(&branch.condition, source, offset) {
                        return Some(name);
                    }
                    for item in &branch.body.items {
                        if let Some(name) = find_in_block_item(item, source, offset) {
                            return Some(name);
                        }
                    }
                    if let Some(ref trailing) = branch.body.trailing {
                        if let Some(name) = find_in_expr(trailing, source, offset) {
                            return Some(name);
                        }
                    }
                }
                if let Some(ref else_branch) = if_expr.else_branch {
                    for item in &else_branch.items {
                        if let Some(name) = find_in_block_item(item, source, offset) {
                            return Some(name);
                        }
                    }
                    if let Some(ref trailing) = else_branch.trailing {
                        if let Some(name) = find_in_expr(trailing, source, offset) {
                            return Some(name);
                        }
                    }
                }
            }
            ExprKind::When(when_expr) => {
                if let Some(name) = find_in_expr(&when_expr.subject, source, offset) {
                    return Some(name);
                }
                for arm in &when_expr.arms {
                    if let Some(name) = find_in_expr(&arm.body, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref else_arm) = when_expr.else_arm {
                    if let Some(name) = find_in_expr(else_arm, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::For(for_expr) => {
                if let Some(name) = find_in_expr(&for_expr.iterable, source, offset) {
                    return Some(name);
                }
                for item in &for_expr.body.items {
                    if let Some(name) = find_in_block_item(item, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref trailing) = for_expr.body.trailing {
                    if let Some(name) = find_in_expr(trailing, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Lambda(lambda) => {
                if let Some(name) = find_in_expr(&lambda.body, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Call { callee, arguments } => {
                if let Some(name) = find_in_expr(callee, source, offset) {
                    return Some(name);
                }
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            if let Some(name) = find_in_expr(e, source, offset) {
                                return Some(name);
                            }
                        }
                    }
                }
            }
            ExprKind::ReceiverCall {
                receiver,
                callee: _,
                arguments,
            } => {
                if let Some(name) = find_in_expr(receiver, source, offset) {
                    return Some(name);
                }
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            if let Some(name) = find_in_expr(e, source, offset) {
                                return Some(name);
                            }
                        }
                    }
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    if let Some(name) = find_in_expr(item, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    if let Some(name) = find_in_expr(&field.value, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Index { target, index } => {
                if let Some(name) = find_in_expr(target, source, offset) {
                    return Some(name);
                }
                if let Some(name) = find_in_expr(index, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => {
                if let Some(name) = find_in_expr(target, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Unary { expr: inner, .. } => {
                if let Some(name) = find_in_expr(inner, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                if let Some(name) = find_in_expr(left, source, offset) {
                    return Some(name);
                }
                if let Some(name) = find_in_expr(right, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Range(range) => {
                if let Some(ref start) = range.start {
                    if let Some(name) = find_in_expr(start, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref end) = range.end {
                    if let Some(name) = find_in_expr(end, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Intrinsic(intr) => match intr {
                IntrinsicExpr::Updated(u) => {
                    if let Some(name) = find_in_expr(&u.target, source, offset) {
                        return Some(name);
                    }
                    for upd in &u.updates {
                        if let Some(name) = find_in_expr(&upd.value, source, offset) {
                            return Some(name);
                        }
                    }
                }
                IntrinsicExpr::Econ(e) => {
                    for item in &e.body.items {
                        if let Some(name) = find_in_block_item(item, source, offset) {
                            return Some(name);
                        }
                    }
                    if let Some(ref trailing) = e.body.trailing {
                        if let Some(name) = find_in_expr(trailing, source, offset) {
                            return Some(name);
                        }
                    }
                }
            },
            ExprKind::Name(_)
            | ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::String(_) => {}
        }

        None
    }

    fn find_in_block_item(item: &BlockItem, source: &str, offset: usize) -> Option<String> {
        match item {
            BlockItem::LocalValue(lv) => find_in_expr(&lv.initializer, source, offset),
            BlockItem::Assignment(a) => find_in_expr(&a.value, source, offset),
            BlockItem::CompoundAssignment(ca) => find_in_expr(&ca.value, source, offset),
            BlockItem::Return(r) => match &r.value {
                Some(val) => find_in_expr(val, source, offset),
                None => None,
            },
            BlockItem::Panic(_) => None,
            BlockItem::BlockStatement(e) | BlockItem::Expr(e) => find_in_expr(e, source, offset),
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                if let Some(name) = find_in_expr(&f.body, source, offset) {
                    return Some(name);
                }
            }
            TopLevelItem::Value(v) => {
                if let Some(name) = find_in_expr(&v.initializer, source, offset) {
                    return Some(name);
                }
            }
            TopLevelItem::Statement(s) => {
                if let Some(name) = find_in_block_item(s, source, offset) {
                    return Some(name);
                }
            }
            TopLevelItem::Param(p) => {
                if let Some(ref default) = p.default {
                    if let Some(name) = find_in_expr(default, source, offset) {
                        return Some(name);
                    }
                }
            }
            TopLevelItem::Import(_) => {}
        }
    }

    if let Some(ref result) = unit.syntax.result {
        if let Some(name) = find_in_expr(result, source, offset) {
            return Some(name);
        }
    }

    None
}

fn compute_goto_definition(source: &str, uri: Url, position: Position) -> Option<GotoDefinitionResponse> {
    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source(&source_text).ok()?;
    let offset = position_to_byte_offset(source, position);
    let name = find_name_at_offset(&unit, source, offset)?;
    let symbols = build_symbol_table(&unit);
    let target_span = symbols.get(&name)?;
    let range = byte_span_to_range(source, target_span.start, target_span.end);
    Some(GotoDefinitionResponse::Scalar(Location { uri, range }))
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
                definition_provider: Some(OneOf::Left(true)),
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

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document_position_params.text_document.uri)
            .cloned();

        match source {
            Some(text) => Ok(compute_goto_definition(
                &text,
                params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )),
            None => Ok(None),
        }
    }
}
